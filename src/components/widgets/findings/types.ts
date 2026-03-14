/**
 * Findings Widget Types
 *
 * Type definitions for the Findings dashboard widget.
 * These types represent AI-detected issues (bugs, security, performance, etc.)
 */

/**
 * Severity level of a finding.
 * Ordered from most to least severe.
 */
export type FindingSeverity = "critical" | "high" | "medium" | "low" | "info";

/**
 * Category of the finding.
 * Indicates what type of issue was detected.
 */
export type FindingCategory =
  | "code_bug"
  | "security"
  | "performance"
  | "todo"
  | "enhancement"
  | "config_issue"
  | "test_issue"
  | "documentation";

/**
 * Current status of the finding.
 */
export type FindingStatus = "detected" | "in_progress" | "needs_input" | "resolved";

/**
 * A single finding detected by AI analysis.
 */
export interface Finding {
  /** Unique identifier for the finding */
  id: string;
  /** Category of the issue */
  category: FindingCategory;
  /** Severity level */
  severity: FindingSeverity;
  /** Current status */
  status: FindingStatus;
  /** Short title describing the issue */
  title: string;
  /** Detailed description of the issue */
  description: string;
  /** Path to the file where the issue was found (optional) */
  filePath?: string;
  /** Line number in the file (optional) */
  lineNumber?: number;
  /** Suggested fix for the issue (optional) */
  suggestedFix?: string;
  /** Timestamp when the finding was detected */
  detectedAt: number;
}

/**
 * Aggregated data for the Findings widget.
 */
export interface FindingsData {
  /** All findings */
  findings: Finding[];
  /** Total number of findings */
  totalCount: number;
  /** Count of findings by severity level */
  bySeverity: Record<FindingSeverity, number>;
  /** Count of findings by status */
  byStatus: Record<FindingStatus, number>;
}
