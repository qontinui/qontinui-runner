/**
 * Issues Types
 *
 * Types for tracking errors and issues detected during AI-assisted automation sessions.
 * These are parsed from AI output using structured markers like [ISSUE:DETECTED].
 */

/** Issue severity levels */
export type IssueSeverity = "critical" | "high" | "medium" | "low";

/** Issue types */
export type IssueType =
  | "error"
  | "warning"
  | "exception"
  | "type_error"
  | "runtime_error";

/** Issue status */
export type IssueStatus = "detected" | "in_progress" | "resolved" | "skipped";

/** Source types - where the AI found the error */
export type IssueSourceType =
  | "log"
  | "screenshot"
  | "console"
  | "test_output"
  | "ai_analysis"
  | "other";

/** Source information - where the AI found/detected the error */
export interface IssueSource {
  type: IssueSourceType;
  /** File path (for logs) or screenshot path */
  path?: string;
  /** Lines in log file where error was found [start, end] */
  line_range?: [number, number];
  /** Human-readable description (e.g., "backend.log", "Playwright screenshot") */
  description?: string;
}

/** A detected issue */
export interface DetectedIssue {
  id: string;
  session_id: string;
  project_id?: string;

  // Issue details
  type: IssueType;
  severity: IssueSeverity;
  title: string;
  description: string;

  // Location (where the error occurs in code)
  file?: string;
  line?: number;

  // Source (where the AI found/detected the error)
  source: IssueSource;

  // Status tracking
  status: IssueStatus;
  resolution?: string;

  // Timestamps
  detected_at: number;
  resolved_at?: number;
}

/** Payload for [ISSUE:DETECTED] marker */
export interface IssueDetectedPayload {
  type: IssueType;
  severity: IssueSeverity;
  title: string;
  description: string;
  file?: string;
  line?: number;
  source: IssueSource;
}

/** Payload for [ISSUE:RESOLVED] marker */
export interface IssueResolvedPayload {
  id: string;
  resolution: string;
}

/** Payload for [ISSUE:PROGRESS] marker */
export interface IssueProgressPayload {
  id: string;
  status: IssueStatus;
}

/** Session issue summary statistics */
export interface IssueSessionSummary {
  session_id: string;
  total: number;
  by_status: {
    detected: number;
    in_progress: number;
    resolved: number;
    skipped: number;
  };
  by_severity: {
    critical: number;
    high: number;
    medium: number;
    low: number;
  };
}

/** Severity configuration for UI display */
export const SEVERITY_CONFIG: Record<
  IssueSeverity,
  { label: string; color: string; bgColor: string }
> = {
  critical: {
    label: "Critical",
    color: "text-red-500",
    bgColor: "bg-red-500/10",
  },
  high: {
    label: "High",
    color: "text-orange-500",
    bgColor: "bg-orange-500/10",
  },
  medium: {
    label: "Medium",
    color: "text-yellow-500",
    bgColor: "bg-yellow-500/10",
  },
  low: {
    label: "Low",
    color: "text-blue-500",
    bgColor: "bg-blue-500/10",
  },
};

/** Status configuration for UI display */
export const STATUS_CONFIG: Record<
  IssueStatus,
  { label: string; color: string; bgColor: string }
> = {
  detected: {
    label: "Detected",
    color: "text-red-400",
    bgColor: "bg-red-500/10",
  },
  in_progress: {
    label: "In Progress",
    color: "text-yellow-400",
    bgColor: "bg-yellow-500/10",
  },
  resolved: {
    label: "Resolved",
    color: "text-green-400",
    bgColor: "bg-green-500/10",
  },
  skipped: {
    label: "Skipped",
    color: "text-gray-400",
    bgColor: "bg-gray-500/10",
  },
};

/** Source type labels for UI display */
export const SOURCE_TYPE_LABELS: Record<IssueSourceType, string> = {
  log: "Log File",
  screenshot: "Screenshot",
  console: "Console",
  test_output: "Test Output",
  ai_analysis: "AI Analysis",
  other: "Other",
};
