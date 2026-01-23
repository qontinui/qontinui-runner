/**
 * Type definitions for the Session Recap page.
 *
 * These types match the Rust structs in commands/recap.rs
 */

/**
 * A step in the recap timeline.
 */
export interface RecapStep {
  /** Step name */
  name: string;
  /** Step type: "workflow", "action", "ai_session", "test", "check" */
  step_type: string;
  /** Status: "success", "failed", "running", "skipped" */
  status: string;
  /** Workflow phase: "setup", "agentic", "verification", "completion" */
  phase?: "setup" | "agentic" | "verification" | "completion";

  /** Icon type for rendering (maps to STEP_ICON_CONFIG). More specific than step_type. */
  icon_type?: string;
  /** AI-generated summary of work done (for AI steps) */
  work_summary?: string;

  /** Brief summary of what happened (deterministic summary for non-AI steps) */
  summary?: string;
  /** Duration in milliseconds */
  duration_ms?: number;
  /** Error message if failed */
  error?: string;
  /** Nested steps (for workflows containing actions) */
  children: RecapStep[];
}

/**
 * Information about why a run failed.
 */
export interface FailureInfo {
  /** Primary reason for failure */
  reason: string;
  /** Name of the step that failed */
  failed_step?: string;
  /** Detailed error message or stack trace */
  error_details?: string;
  /** Error type category */
  error_type?: string;
}

/**
 * Quick statistics about the run.
 */
export interface RecapStats {
  /** Total number of actions executed */
  total_actions: number;
  /** Number of successful actions */
  successful_actions: number;
  /** Number of failed actions */
  failed_actions: number;
  /** Number of skipped actions */
  skipped_actions: number;
  /** Total number of AI sessions */
  ai_sessions: number;
  /** Number of tests run */
  tests_run: number;
  /** Number of tests passed */
  tests_passed: number;
}

/**
 * A stage in the recap timeline, grouping related steps.
 * Stages represent the 4 workflow phases: setup, agentic, verification, completion.
 */
export interface StageRecap {
  /** Stage identifier: "setup", "agentic", "verification", "completion" */
  stage: "setup" | "agentic" | "verification" | "completion";
  /** Display name: "Setup", "Agentic", "Verification", "Completion" */
  display_name: string;
  /** Status: "success", "failed", "running", "skipped", "pending" */
  status: "success" | "failed" | "running" | "skipped" | "pending";
  /** When this stage started (ISO 8601) */
  started_at?: string;
  /** When this stage ended (ISO 8601) */
  ended_at?: string;
  /** Duration in milliseconds */
  duration_ms?: number;
  /** Steps in this stage */
  steps: RecapStep[];
  /** Iteration number (for agentic/verification in loop) */
  iteration?: number;
}

/**
 * Complete recap data for a task run.
 */
export interface RecapData {
  /** Task run ID */
  task_run_id: string;
  /** Task name */
  task_name: string;
  /** Overall status: "running", "complete", "failed", "stopped" */
  status: string;
  /** Total duration in milliseconds */
  duration_ms?: number;
  /** When the run started (ISO 8601) */
  created_at: string;
  /** When the run completed (ISO 8601) */
  completed_at?: string;

  /** Failure info (prominent if failed) */
  failure_info?: FailureInfo;

  /** AI-generated or extracted summary */
  summary?: string;

  /** Whether the goal was achieved (from orchestrator) */
  goal_achieved?: boolean;

  /** Steps grouped by stage (with timing from transition_history) */
  stages: StageRecap[];

  /** Steps overview (timeline) - flat list for backwards compatibility */
  steps: RecapStep[];

  /** Quick statistics */
  stats: RecapStats;
}

/**
 * Response wrapper from the recap command.
 */
export interface RecapResponse {
  success: boolean;
  data?: RecapData;
  error?: string;
}

// ============================================================================
// Verification Results Types (Section 5)
// ============================================================================

/**
 * A verification criterion result.
 */
export interface VerificationCriterion {
  /** Criterion ID */
  id: string;
  /** Criterion description */
  description: string;
  /** Type: "deterministic" or "ai_evaluated" */
  type: "deterministic" | "ai_evaluated";
  /** Status: "passed", "failed", "skipped" */
  status: "passed" | "failed" | "skipped";
  /** Confidence level (0-1, for AI evaluated) */
  confidence?: number;
  /** Observations from evaluation */
  observations?: string[];
  /** Issues found */
  issues?: string[];
  /** Suggestions for improvement */
  suggestions?: string[];
}

/**
 * Verification plan summary.
 */
export interface VerificationPlan {
  /** Goal summary */
  goal_summary: string;
  /** All criteria */
  criteria: VerificationCriterion[];
}

// ============================================================================
// Knowledge & Findings Types (Section 6)
// ============================================================================

/**
 * A finding with severity and category.
 */
export interface Finding {
  /** Finding ID */
  id: string;
  /** Category: code_bug, security, test_issue, documentation, etc. */
  category: string;
  /** Severity: critical, high, medium, low, info */
  severity: "critical" | "high" | "medium" | "low" | "info";
  /** Title */
  title: string;
  /** Description */
  description: string;
  /** File path (if applicable) */
  file_path?: string;
  /** Line number (if applicable) */
  line?: number;
  /** Resolution (if fixed) */
  resolution?: string;
  /** Status: open, resolved, dismissed */
  status: "open" | "resolved" | "dismissed";
}

/**
 * A knowledge entry from the task.
 */
export interface KnowledgeEntry {
  /** Entry ID */
  id: string;
  /** Category: findings, root_cause, observation, hypothesis, solution, context */
  category: string;
  /** Agent type: planning, worker, verification, system */
  agent_type: string;
  /** Iteration number */
  iteration: number;
  /** Content */
  content: string;
  /** Evidence */
  evidence?: string;
  /** Confidence level */
  confidence?: number;
  /** Created at timestamp */
  created_at: string;
}

// ============================================================================
// Automation Metrics Types (Section 7)
// ============================================================================

/**
 * Automation action statistics.
 */
export interface ActionStats {
  /** Total actions */
  total: number;
  /** Successful actions */
  success: number;
  /** Failed actions */
  failed: number;
  /** Skipped actions */
  skipped: number;
}

/**
 * A state visited during automation.
 */
export interface StateVisit {
  /** State name */
  name: string;
  /** Visit count */
  visits: number;
  /** Time spent (ms) */
  time_spent_ms?: number;
}

/**
 * A state transition.
 */
export interface StateTransition {
  /** From state */
  from: string;
  /** To state */
  to: string;
  /** Transition count */
  count: number;
}

/**
 * Template match result.
 */
export interface TemplateMatch {
  /** Template name */
  template: string;
  /** Match confidence */
  confidence: number;
  /** Screenshot path */
  screenshot_path?: string;
  /** Match location */
  location?: { x: number; y: number; width: number; height: number };
}

// ============================================================================
// Test Results Types (Section 8)
// ============================================================================

/**
 * A test result.
 */
export interface TestResult {
  /** Test result ID */
  id: string;
  /** Test name */
  name: string;
  /** Status: passed, failed, skipped */
  status: "passed" | "failed" | "skipped";
  /** Duration in ms */
  duration_ms?: number;
  /** Assertions passed */
  assertions_passed: number;
  /** Assertions failed */
  assertions_failed: number;
  /** Console output */
  console_output?: string;
  /** Page snapshot (YAML) */
  page_snapshot?: string;
  /** Failure screenshots */
  screenshots?: string[];
  /** Error message */
  error_message?: string;
  /** Stack trace */
  stack_trace?: string;
}

// ============================================================================
// Execution Context Types (Section 9)
// ============================================================================

/**
 * A context variable.
 */
export interface ContextVariable {
  /** Variable name */
  name: string;
  /** Variable value */
  value: string;
  /** Source: user, system, step */
  source: string;
  /** Source step name (if from a step) */
  source_step?: string;
}

/**
 * A retry attempt.
 */
export interface RetryAttempt {
  /** Attempt number */
  attempt: number;
  /** Error message */
  error: string;
  /** Timestamp */
  timestamp: string;
  /** Delay before next attempt (ms) */
  delay_ms?: number;
}

/**
 * Execution context data.
 */
export interface ExecutionContext {
  /** Context variables */
  variables: ContextVariable[];
  /** Retry history */
  retry_history: RetryAttempt[];
}
