/**
 * Execution Status Types
 *
 * Type definitions for the agentic execution status system.
 * Used to display real-time visibility into task routing, retries,
 * memory compression, and lifecycle hooks.
 */

// ============================================================================
// Task Routing Types
// ============================================================================

/** Complexity level of a task */
export type TaskComplexity = "simple" | "medium" | "complex";

/** Factors that influenced the routing decision */
export interface RoutingFactor {
  /** Description of the factor */
  description: string;
  /** Complexity level this factor points to */
  complexity: TaskComplexity;
  /** Weight of this factor (0-1) */
  weight: number;
}

/** Result of task routing assessment */
export interface RoutingDecision {
  /** The assessed complexity level */
  complexity: TaskComplexity;
  /** Confidence in the assessment (0-1) */
  confidence: number;
  /** Factors that contributed to this assessment */
  factors: string[];
  /** The model selected based on this complexity */
  selectedModel: string;
  /** Timestamp of the decision */
  timestamp: number;
  /** Task prompt preview (truncated) */
  promptPreview?: string;
  /** File count if analyzed */
  fileCount?: number;
  /** Criteria count if analyzed */
  criteriaCount?: number;
}

/** Routing status for display */
export interface RoutingStatus {
  /** Whether routing is enabled */
  enabled: boolean;
  /** Current or last routing decision */
  decision: RoutingDecision | null;
  /** Configuration summary */
  config: {
    simpleModel: string;
    mediumModel: string;
    complexModel: string;
  };
}

// ============================================================================
// Retry Status Types
// ============================================================================

/** Record of a single retry attempt */
export interface RetryAttempt {
  /** Attempt number (1-indexed) */
  attemptNumber: number;
  /** Error that triggered this retry */
  error: string;
  /** When the retry was attempted */
  timestamp: string;
  /** Delay before this retry (ms) */
  delayMs: number;
  /** Whether feedback was injected for this retry */
  feedbackInjected: boolean;
}

/** Current retry state */
export interface RetryState {
  /** Current attempt number (0 = initial attempt) */
  attempt: number;
  /** Last error encountered */
  lastError: string | null;
  /** Timestamp of last attempt */
  lastAttemptAt: string | null;
  /** Total delay accumulated (ms) */
  totalDelayMs: number;
  /** History of all retry attempts */
  errorHistory: RetryAttempt[];
}

/** Retry status for display */
export interface RetryStatus {
  /** Whether retry is enabled */
  enabled: boolean;
  /** Maximum retries allowed */
  maxRetries: number;
  /** Whether feedback injection is enabled */
  feedbackInjection: boolean;
  /** Current retry state */
  state: RetryState;
  /** Next retry delay (ms) if another retry will happen */
  nextRetryDelayMs: number | null;
  /** Whether retries are exhausted */
  exhausted: boolean;
}

// ============================================================================
// Memory Compression Types
// ============================================================================

/** Token count breakdown by category */
export interface TokenCount {
  /** Total estimated tokens */
  total: number;
  /** Tokens from findings */
  findings: number;
  /** Tokens from observations */
  observations: number;
  /** Tokens from verification feedback */
  feedback: number;
  /** Tokens from solutions */
  solutions: number;
  /** Tokens from other categories */
  other: number;
  /** Number of knowledge entries counted */
  entryCount: number;
}

/** Result of a compression operation */
export interface CompressionResult {
  /** Token count before compression */
  originalTokens: number;
  /** Token count after compression */
  compressedTokens: number;
  /** Number of items that were summarized */
  itemsSummarized: number;
  /** Number of summary entries created */
  summaryEntriesCreated: number;
  /** Categories that were compressed */
  compressedCategories: string[];
  /** Timestamp of compression */
  timestamp: string;
}

/** Compression status for display */
export interface CompressionStatus {
  /** Whether compression is enabled */
  enabled: boolean;
  /** Token threshold that triggers compression */
  thresholdTokens: number;
  /** Target tokens after compression */
  targetTokens: number;
  /** Current estimated token count */
  currentTokenCount: TokenCount | null;
  /** Last compression result */
  lastCompression: CompressionResult | null;
  /** Percentage of threshold used */
  thresholdPercentage: number;
  /** Whether compression is imminent */
  compressionImminent: boolean;
}

// ============================================================================
// Lifecycle Hook Types
// ============================================================================

/** Hook trigger events */
export type HookTrigger =
  | "pre_execution"
  | "post_execution"
  | "on_error"
  | "on_verification_fail"
  | "on_complete"
  | "pre_iteration"
  | "post_iteration";

/** Result of hook execution */
export interface HookExecutionResult {
  /** Hook ID */
  hookId: string;
  /** Hook name */
  hookName: string;
  /** When this hook triggers */
  trigger: HookTrigger;
  /** Whether execution succeeded */
  success: boolean;
  /** Output from the hook */
  output: string | null;
  /** Error message if failed */
  error: string | null;
  /** Execution duration (ms) */
  durationMs: number;
  /** Timestamp of execution */
  timestamp: string;
}

/** Definition of a hook for display */
export interface HookDefinition {
  /** Unique identifier */
  id: string;
  /** Human-readable name */
  name: string;
  /** When this hook triggers */
  trigger: HookTrigger;
  /** Whether the hook is enabled */
  enabled: boolean;
  /** Action type (command, webhook, log, notification) */
  actionType: string;
}

/** Hook execution status for display */
export interface HookStatus {
  /** Configured hooks */
  hooks: HookDefinition[];
  /** Execution history */
  executionHistory: HookExecutionResult[];
  /** Currently executing hook (if any) */
  currentlyExecuting: string | null;
}

// ============================================================================
// Sub-Step Progress Types
// ============================================================================

/** Status of a sub-step */
export type SubStepStatus = "pending" | "running" | "completed" | "failed" | "skipped";

/** Information about a single sub-step */
export interface SubStepInfo {
  /** Unique identifier for the sub-step */
  id: string;
  /** Parent checkpoint or action ID */
  parentId: string;
  /** Human-readable name/description */
  name: string;
  /** Current status */
  status: SubStepStatus;
  /** Index within the parent (0-based) */
  index: number;
  /** Total number of sub-steps in parent */
  totalCount: number;
  /** When the sub-step started */
  startedAt: number | null;
  /** When the sub-step completed */
  completedAt: number | null;
  /** Duration in milliseconds */
  durationMs: number | null;
  /** Phase this sub-step belongs to */
  phase?: string;
}

/** Sub-step progress status for display */
export interface SubStepStatus_Display {
  /** Current sub-step being executed */
  current: SubStepInfo | null;
  /** All sub-steps in the current batch */
  steps: SubStepInfo[];
  /** Overall progress (0-100) */
  progressPercent: number;
  /** Number of completed sub-steps */
  completedCount: number;
  /** Total number of sub-steps */
  totalCount: number;
  /** Whether sub-step tracking is active */
  isActive: boolean;
  /** Phase currently executing */
  currentPhase: string | null;
}

/** Raw sub-step complete event from backend */
export interface RawSubStepCompleteEvent {
  type: "sub_step_complete";
  checkpoint_id: string;
  task_run_id: string;
  sub_step_id: string;
  description: string | null;
  timestamp: number;
}

/** Raw sub-step started event from backend */
export interface RawSubStepStartedEvent {
  type: "sub_step_started";
  checkpoint_id: string;
  task_run_id: string;
  sub_step_id: string;
  sub_step_index: number;
  total_sub_steps: number;
  description: string | null;
  phase: string | null;
  timestamp: number;
}

// ============================================================================
// Combined Execution Status
// ============================================================================

/** Combined execution status for the panel */
export interface ExecutionStatus {
  /** Task run ID */
  taskRunId: string | null;
  /** Task name */
  taskName: string | null;
  /** Current iteration */
  iteration: number;
  /** Overall status */
  status: "idle" | "running" | "completed" | "failed" | "paused";
  /** Task routing status */
  routing: RoutingStatus;
  /** Retry status */
  retry: RetryStatus;
  /** Memory compression status */
  compression: CompressionStatus;
  /** Lifecycle hooks status */
  hooks: HookStatus;
  /** Sub-step progress status */
  subSteps: SubStepStatus_Display;
  /** Last updated timestamp */
  lastUpdated: number;
}

// ============================================================================
// Event Types for Real-Time Updates
// ============================================================================

// ============================================================================
// Raw Event Types (snake_case to match backend)
// ============================================================================

/** Base event structure (from backend) */
export interface RawExecutionStatusEventBase {
  /** Event type */
  type: string;
  /** Task run ID */
  task_run_id: string;
  /** Timestamp */
  timestamp: number;
}

/** Raw routing decision payload */
export interface RawRoutingDecisionPayload {
  complexity: string;
  confidence: number;
  factors: string[];
  selected_model: string;
  prompt_preview?: string;
  file_count?: number;
  criteria_count?: number;
}

/** Raw retry attempt payload */
export interface RawRetryAttemptPayload {
  attempt_number: number;
  error: string;
  attempt_timestamp: string;
  delay_ms: number;
  feedback_injected: boolean;
}

/** Raw retry state payload */
export interface RawRetryStatePayload {
  attempt: number;
  last_error: string | null;
  last_attempt_at: string | null;
  total_delay_ms: number;
  error_history: RawRetryAttemptPayload[];
}

/** Raw token count payload */
export interface RawTokenCountPayload {
  total: number;
  findings: number;
  observations: number;
  feedback: number;
  solutions: number;
  other: number;
  entry_count: number;
}

/** Raw compression result payload */
export interface RawCompressionResultPayload {
  original_tokens: number;
  compressed_tokens: number;
  items_summarized: number;
  summary_entries_created: number;
  compressed_categories: string[];
  timestamp: string;
}

/** Raw hook execution payload */
export interface RawHookExecutionPayload {
  hook_id: string;
  hook_name: string;
  trigger: string;
  success: boolean;
  output: string | null;
  error: string | null;
  duration_ms: number;
  timestamp: string;
}

/** Routing decision event (from backend) */
export interface RawRoutingDecisionEvent extends RawExecutionStatusEventBase {
  type: "routing_decision";
  decision: RawRoutingDecisionPayload;
}

/** Retry attempt event (from backend) */
export interface RawRetryAttemptEvent extends RawExecutionStatusEventBase {
  type: "retry_attempt";
  attempt: RawRetryAttemptPayload;
  state: RawRetryStatePayload;
  exhausted: boolean;
  next_retry_delay_ms: number | null;
}

/** Compression event (from backend) */
export interface RawCompressionEvent extends RawExecutionStatusEventBase {
  type: "compression";
  result: RawCompressionResultPayload;
  current_token_count: RawTokenCountPayload;
}

/** Token count update event (from backend) */
export interface RawTokenCountUpdateEvent extends RawExecutionStatusEventBase {
  type: "token_count_update";
  token_count: RawTokenCountPayload;
  threshold_percentage: number;
  compression_imminent: boolean;
}

/** Hook execution event (from backend) */
export interface RawHookExecutionEvent extends RawExecutionStatusEventBase {
  type: "hook_execution";
  result: RawHookExecutionPayload;
}

/** Hook started event (from backend) */
export interface RawHookStartedEvent extends RawExecutionStatusEventBase {
  type: "hook_started";
  hook_id: string;
  hook_name: string;
  trigger: string;
}

/** Status change event (from backend) */
export interface RawStatusChangeEvent extends RawExecutionStatusEventBase {
  type: "status_change";
  status: string;
  iteration: number;
  task_name: string | null;
}

/** Union of all raw execution status events */
export type RawExecutionStatusEvent =
  | RawRoutingDecisionEvent
  | RawRetryAttemptEvent
  | RawCompressionEvent
  | RawTokenCountUpdateEvent
  | RawHookExecutionEvent
  | RawHookStartedEvent
  | RawStatusChangeEvent;

// ============================================================================
// Default Values
// ============================================================================

/** Create default routing status */
export function createDefaultRoutingStatus(): RoutingStatus {
  return {
    enabled: false,
    decision: null,
    config: {
      simpleModel: "claude-3-5-haiku-20241022",
      mediumModel: "claude-sonnet-4-20250514",
      complexModel: "claude-opus-4-20250514",
    },
  };
}

/** Create default retry status */
export function createDefaultRetryStatus(): RetryStatus {
  return {
    enabled: true,
    maxRetries: 3,
    feedbackInjection: true,
    state: {
      attempt: 0,
      lastError: null,
      lastAttemptAt: null,
      totalDelayMs: 0,
      errorHistory: [],
    },
    nextRetryDelayMs: null,
    exhausted: false,
  };
}

/** Create default compression status */
export function createDefaultCompressionStatus(): CompressionStatus {
  return {
    enabled: true,
    thresholdTokens: 80000,
    targetTokens: 60000,
    currentTokenCount: null,
    lastCompression: null,
    thresholdPercentage: 0,
    compressionImminent: false,
  };
}

/** Create default hook status */
export function createDefaultHookStatus(): HookStatus {
  return {
    hooks: [],
    executionHistory: [],
    currentlyExecuting: null,
  };
}

/** Create default sub-step status */
export function createDefaultSubStepStatus(): SubStepStatus_Display {
  return {
    current: null,
    steps: [],
    progressPercent: 0,
    completedCount: 0,
    totalCount: 0,
    isActive: false,
    currentPhase: null,
  };
}

/** Create default execution status */
export function createDefaultExecutionStatus(): ExecutionStatus {
  return {
    taskRunId: null,
    taskName: null,
    iteration: 0,
    status: "idle",
    routing: createDefaultRoutingStatus(),
    retry: createDefaultRetryStatus(),
    compression: createDefaultCompressionStatus(),
    hooks: createDefaultHookStatus(),
    subSteps: createDefaultSubStepStatus(),
    lastUpdated: Date.now(),
  };
}

// ============================================================================
// Helper Functions
// ============================================================================

/** Get display name for complexity level */
export function getComplexityDisplayName(complexity: TaskComplexity): string {
  const names: Record<TaskComplexity, string> = {
    simple: "Simple",
    medium: "Medium",
    complex: "Complex",
  };
  return names[complexity];
}

/** Get display name for hook trigger */
export function getHookTriggerDisplayName(trigger: HookTrigger): string {
  const names: Record<HookTrigger, string> = {
    pre_execution: "Pre-Execution",
    post_execution: "Post-Execution",
    on_error: "On Error",
    on_verification_fail: "On Verification Fail",
    on_complete: "On Complete",
    pre_iteration: "Pre-Iteration",
    post_iteration: "Post-Iteration",
  };
  return names[trigger];
}

/** Format duration in milliseconds to human readable */
export function formatDurationMs(ms: number): string {
  if (ms < 1000) {
    return `${ms}ms`;
  } else if (ms < 60000) {
    return `${(ms / 1000).toFixed(1)}s`;
  } else {
    const minutes = Math.floor(ms / 60000);
    const seconds = Math.floor((ms % 60000) / 1000);
    return `${minutes}m ${seconds}s`;
  }
}

/** Format token count with K suffix */
export function formatTokenCount(tokens: number): string {
  if (tokens < 1000) {
    return tokens.toString();
  } else if (tokens < 10000) {
    return `${(tokens / 1000).toFixed(1)}K`;
  } else {
    return `${Math.round(tokens / 1000)}K`;
  }
}

/** Calculate compression savings percentage */
export function calculateCompressionSavings(result: CompressionResult): number {
  if (result.originalTokens === 0) return 0;
  return ((result.originalTokens - result.compressedTokens) / result.originalTokens) * 100;
}
