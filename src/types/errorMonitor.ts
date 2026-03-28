/**
 * errorMonitor.ts
 *
 * Type definitions for the error monitoring system.
 * These types mirror the Rust types in src-tauri/src/error_monitor/types.rs
 */

// =============================================================================
// Enums
// =============================================================================

/** Error severity levels */
export type ErrorSeverity = "critical" | "error" | "warning" | "info" | "debug";

/** Error resolution status */
export type ErrorStatus =
  | "new"
  | "acknowledged"
  | "in_progress"
  | "resolved"
  | "ignored"
  | "recurring"
  | "promoted";

/** Parser type for log sources */
export type ParserType = "python" | "javascript" | "rust" | "generic";

/** Path type for log sources */
export type PathType = "file" | "glob" | "directory";

/** Log format for log sources */
export type LogFormat = "plaintext" | "json" | "jsonl";

// =============================================================================
// Log Source Configuration
// =============================================================================

/** Configuration for a log source */
export interface LogSourceConfig {
  /** Unique identifier (set by database) */
  id?: number;
  /** Human-readable name */
  name: string;
  /** Optional description */
  description?: string;
  /** Path to log file, glob pattern, or directory */
  path: string;
  /** Type of path */
  pathType: PathType;
  /** Format of the log file */
  format: LogFormat;
  /** Parser type for this source */
  parser: ParserType;
  /** Regex pattern to extract timestamp from log lines */
  timestampPattern?: string;
  /** Timezone for parsing timestamps */
  timezone?: string;
  /** Custom regex patterns to identify errors */
  errorPatterns?: string[];
  /** Custom regex patterns to identify warnings */
  warningPatterns?: string[];
  /** Patterns to ignore */
  ignorePatterns?: string[];
  /** Whether this source is enabled */
  enabled: boolean;
  /** Polling interval in milliseconds */
  pollIntervalMs?: number;
  /** When this source was created */
  createdAt?: string;
  /** When this source was last updated */
  updatedAt?: string;
}

// =============================================================================
// Error Event Types
// =============================================================================

/** Location information for an error */
export interface ErrorLocation {
  /** Source file path */
  filePath: string;
  /** Line number (if available) */
  lineNumber?: number;
  /** Column number (if available) */
  columnNumber?: number;
  /** Function or method name */
  functionName?: string;
}

/** A stored error event from the database */
export interface StoredErrorEvent {
  /** Unique identifier */
  id: number;
  /** Log source this error came from */
  logSourceId?: number;
  /** Log source name for display */
  logSourceName: string;
  /** Task run this error belongs to (if collected during workflow) */
  taskRunId?: string;
  /** Workflow name (from the task_runs table) */
  workflowName?: string;
  /** Workflow step ID */
  workflowStepId?: string;
  /** Timestamp from log entry */
  logTimestamp?: string;
  /** When the error was captured */
  capturedAt: string;
  /** Error severity */
  severity: ErrorSeverity;
  /** Error type/category */
  errorType?: string;
  /** Error code */
  errorCode?: string;
  /** Error message */
  message: string;
  /** Stack trace if available */
  stackTrace?: string;
  /** Context lines around the error */
  contextLines?: string;
  /** Raw log entry */
  rawEntry?: string;
  /** Location information */
  location?: ErrorLocation;
  /** Hash for deduplication */
  signatureHash: string;
  /** Number of occurrences */
  occurrenceCount: number;
  /** First seen timestamp */
  firstSeenAt: string;
  /** Last seen timestamp */
  lastSeenAt: string;
  /** Resolution status */
  status: ErrorStatus;
  /** Notes about resolution */
  resolutionNotes?: string;
  /** Task run that resolved this error */
  resolvedByTaskRunId?: string;
  /** Linked finding ID */
  linkedFindingId?: number;
}

// =============================================================================
// Query Types
// =============================================================================

/** Query parameters for filtering errors */
export interface ErrorQuery {
  /** Filter by task run */
  taskRunId?: string;
  /** Filter by log source name */
  logSourceName?: string;
  /** Filter by severity levels */
  severity?: ErrorSeverity[];
  /** Filter by statuses */
  status?: ErrorStatus[];
  /** Filter by error type */
  errorType?: string;
  /** Filter errors captured after this time */
  capturedAfter?: string;
  /** Filter errors captured before this time */
  capturedBefore?: string;
  /** Maximum results to return */
  limit?: number;
  /** Offset for pagination */
  offset?: number;
}

// =============================================================================
// Summary Types
// =============================================================================

/** Summary statistics for errors */
export interface ErrorSummary {
  /** Total error count */
  total: number;
  /** New (unreviewed) errors */
  newCount: number;
  /** Total unresolved errors */
  unresolvedCount: number;
  /** Critical severity count */
  criticalCount: number;
  /** Error severity count */
  errorCount: number;
  /** Warning severity count */
  warningCount: number;
  /** Count by log source */
  bySource: Record<string, number>;
  /** Count by error type */
  byErrorType: Record<string, number>;
  /** Count by status */
  byStatus: Record<string, number>;
  /** Whether there are actionable (critical/error, unresolved) errors */
  hasActionableErrors: boolean;
}

// =============================================================================
// Debug Context Types
// =============================================================================

/** Types of error patterns */
export type PatternType =
  | "repeated_error_type"
  | "same_file_location"
  | "same_source"
  | "similar_message"
  | "cascading_failure"
  | "similar_embedding";

/** A pattern detected in errors */
export interface ErrorPattern {
  /** Pattern name */
  name: string;
  /** Number of matches */
  matchCount: number;
  /** Error IDs that match this pattern */
  errorIds: number[];
  /** Frequency (how many errors match) */
  frequency: number;
  /** Representative error IDs (legacy alias) */
  sampleIds: number[];
  /** Pattern type */
  patternType: PatternType;
  /** Suggested root cause */
  suggestedCause?: string;
}

/** Curated debug context for AI */
export interface DebugContext {
  /** Total error count */
  totalCount: number;
  /** Critical errors (highest priority) */
  criticalErrors: StoredErrorEvent[];
  /** Regular errors */
  errors: StoredErrorEvent[];
  /** Warnings */
  warnings: StoredErrorEvent[];
  /** Detected patterns */
  patterns: ErrorPattern[];
  /** Focus areas for investigation */
  focusAreas: string[];
  /** Investigation hints */
  investigationHints: string[];
}

// =============================================================================
// Workflow Types
// =============================================================================

/** Configuration for error fix workflow generation */
export interface ErrorFixWorkflowConfig {
  /** Name for the generated workflow */
  name: string;
  /** Maximum iterations for the debug agent */
  maxIterations: number;
  /** Whether to include warnings in the fix scope */
  includeWarnings: boolean;
  /** Specific error IDs to focus on */
  errorIds: number[];
  /** Task run ID to scope errors to */
  taskRunId?: string;
  /** Additional context to provide to the debug agent */
  additionalContext?: string;
}

/** Generated workflow result */
export interface GeneratedWorkflow {
  /** The unified workflow configuration as JSON */
  workflowJson: Record<string, unknown>;
  /** Human-readable description */
  description: string;
  /** Number of errors targeted */
  errorCount: number;
  /** Whether there are critical errors */
  hasCritical: boolean;
}

/** Summary of fixable errors */
export interface FixableErrorsSummary {
  /** Total number of fixable errors */
  total: number;
  /** Number of critical errors */
  criticalCount: number;
  /** Number of regular errors */
  errorCount: number;
  /** Number of warnings */
  warningCount: number;
  /** Whether a fix workflow can be generated */
  canGenerateWorkflow: boolean;
  /** Recommended action */
  recommendedAction: string;
}
