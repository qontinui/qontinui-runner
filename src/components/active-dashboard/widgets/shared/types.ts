/**
 * Shared Types for Step Widgets
 *
 * Common type definitions used across all step-type widgets.
 */

/**
 * Status of a step execution.
 */
export type StepExecutionStatus = "pending" | "running" | "success" | "failed" | "skipped";

/**
 * Base interface for step execution data.
 * Extended by specific step types with their own fields.
 */
export interface StepExecution {
  /** Unique execution ID */
  id: string;
  /** Display name of the step */
  name: string;
  /** Execution status */
  status: StepExecutionStatus;
  /** Start timestamp (ms since epoch) */
  startTime?: number;
  /** End timestamp (ms since epoch) */
  endTime?: number;
  /** Duration in milliseconds */
  durationMs?: number;
  /** Error message if failed */
  error?: string;
  /** Output/result of the step */
  output?: string;
}

/**
 * Common statistics for step widgets.
 */
export interface StepStats {
  /** Total number of steps */
  total: number;
  /** Number of completed steps (success or failed) */
  completed: number;
  /** Number of successful steps */
  successful: number;
  /** Number of failed steps */
  failed: number;
  /** Number of pending/running steps */
  pending: number;
  /** Total elapsed time in seconds */
  elapsedTime: number;
  /** Success rate percentage (0-100) */
  successRate: number;
}

/**
 * Shell command specific execution data.
 */
export interface ShellCommandExecution extends StepExecution {
  /** The command that was executed (resolved, with variables expanded) */
  command: string;
  /** Working directory */
  workingDirectory?: string;
  /** Exit code */
  exitCode?: number;
  /** Standard output */
  stdout?: string;
  /** Standard error */
  stderr?: string;
  /** Original command template (with {{variable}} placeholders) - only present if variables were used */
  templateCommand?: string;
  /** Variables that were resolved during command execution (variable name -> resolved value) */
  resolvedVariables?: Record<string, string>;
}

/**
 * API request specific execution data.
 */
export interface ApiRequestExecution extends StepExecution {
  /** HTTP method */
  method: string;
  /** Request URL */
  url: string;
  /** Request headers */
  requestHeaders?: Record<string, string>;
  /** Request body */
  requestBody?: string;
  /** Response status code */
  statusCode?: number;
  /** Response headers */
  responseHeaders?: Record<string, string>;
  /** Response body */
  responseBody?: string;
  /** Content type */
  contentType?: string;
}

/**
 * Script specific execution data.
 */
export interface ScriptExecution extends StepExecution {
  /** Script language (python, javascript, etc.) */
  language: string;
  /** Script content (may be truncated for display) */
  scriptContent?: string;
  /** Script file path if loaded from file */
  scriptPath?: string;
  /** Exit code */
  exitCode?: number;
  /** Standard output */
  stdout?: string;
  /** Standard error */
  stderr?: string;
}

/**
 * Workflow reference specific execution data.
 */
export interface WorkflowRefExecution extends StepExecution {
  /** Referenced workflow ID */
  workflowId: string;
  /** Referenced workflow name */
  workflowName: string;
  /** Number of steps in the referenced workflow */
  totalSteps: number;
  /** Number of completed steps */
  completedSteps: number;
  /** Sub-step execution details (optional, for expansion) */
  subSteps?: StepExecution[];
}
