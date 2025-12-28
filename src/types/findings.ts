/**
 * findings.ts
 *
 * Type definitions for the categorized findings system.
 * This replaces the simpler "issues" system with a more comprehensive
 * approach that categorizes detected items appropriately.
 */

// Built-in category IDs
export type BuiltInCategoryId =
  | "code_bug" // Actual code issues - auto-fixable
  | "todo" // TODOs - may need user input
  | "config_issue" // Configuration/environment problems
  | "already_fixed" // Resolved in previous sessions
  | "expected_behavior" // By design, not a bug
  | "data_migration" // Requires admin intervention
  | "runtime_issue" // Operational issues, not code
  | "test_issue" // Problems with test code
  | "enhancement" // Improvement suggestions
  | "warning" // Things to be aware of
  | "documentation" // Doc issues/improvements
  | "security" // Security concerns
  | "performance"; // Performance issues

// Action types determine how findings can be handled
export type ActionType =
  | "auto_fix" // Can be fixed by AI without user input
  | "needs_user_input" // AI needs to ask questions first
  | "manual" // User must handle manually
  | "informational"; // No action needed, just FYI

// Finding status tracks the lifecycle of a finding
export type FindingStatus =
  | "detected" // Just found
  | "in_progress" // Being worked on
  | "needs_input" // Waiting for user input
  | "resolved" // Fixed/handled
  | "wont_fix" // Intentionally not fixing
  | "deferred"; // Will handle later

// Severity levels for findings
export type FindingSeverity = "critical" | "high" | "medium" | "low" | "info";

// User input types for questions
export type UserInputType = "text" | "choice" | "boolean" | "code";

/**
 * A category defines how findings are grouped and displayed
 */
export interface FindingCategory {
  id: string;
  name: string;
  description: string;
  icon: string; // Lucide icon name
  color: string; // Tailwind color class (e.g., "red", "amber", "green")
  isBuiltIn: boolean;
  defaultActionType: ActionType;
  sortOrder: number;
}

/**
 * An option for user input questions
 */
export interface UserInputOption {
  value: string;
  label: string;
  description?: string;
}

/**
 * A request for user input on a finding
 */
export interface UserInputRequest {
  id: string;
  findingId: string;
  question: string;
  context?: string; // Additional context for the user
  inputType: UserInputType;
  options?: UserInputOption[];
  required: boolean;
  defaultValue?: string;
}

/**
 * Code context for a finding
 */
export interface CodeContext {
  file?: string;
  line?: number;
  endLine?: number;
  snippet?: string;
}

/**
 * A finding represents a detected item that may or may not need action
 */
export interface Finding {
  id: string;
  categoryId: string;
  severity: FindingSeverity;
  status: FindingStatus;
  title: string;
  description: string;

  // Source tracking
  sourceSessionId?: string;
  sourcePromptName?: string;
  detectedAt: number;

  // Action handling
  actionType: ActionType;
  actionable: boolean;

  // For items needing user input
  pendingQuestion?: UserInputRequest;
  userResponse?: string;

  // Resolution
  resolvedAt?: number;
  resolution?: string;

  // Context for AI
  codeContext?: CodeContext;

  // Extensible metadata
  metadata?: Record<string, unknown>;
}

/**
 * Summary statistics for a category in a report
 */
export interface CategorySummary {
  count: number;
  actionable: number;
  resolved: number;
}

/**
 * Summary statistics for an execution report
 */
export interface ReportSummary {
  totalFindings: number;
  byCategory: Record<string, CategorySummary>;
  bySeverity: Record<FindingSeverity, number>;
  byStatus: Record<FindingStatus, number>;
  actionable: number;
  needsUserInput: number;
  autoFixed: number;
  informational: number;
}

/**
 * Information about a phase/iteration in an execution
 */
export interface PhaseInfo {
  phase: number;
  startedAt: number;
  completedAt?: number;
  findingsDetected: number;
  findingsResolved: number;
}

/**
 * Report status
 */
export type ReportStatus = "running" | "completed" | "paused_for_input" | "failed" | "cancelled";

/**
 * An execution report groups findings from a prompt execution
 */
export interface ExecutionReport {
  id: string;
  sessionId: string;
  promptName: string;
  promptId?: string;

  // Timing
  startedAt: number;
  completedAt?: number;
  duration?: number;

  // Status
  status: ReportStatus;

  // Findings organized by category
  findings: Finding[];

  // Summary statistics
  summary: ReportSummary;

  // Pending user inputs (when paused)
  pendingInputs: UserInputRequest[];

  // Phases/iterations
  phases: PhaseInfo[];
}

/**
 * Parsed finding from AI structured output
 */
export interface ParsedFinding {
  categoryId: string;
  severity: FindingSeverity;
  title: string;
  description: string;
  needsInput: boolean;
  question?: string;
  options?: string[];
  file?: string;
  line?: number;
  resolution?: string;
}

/**
 * Storage for custom categories
 */
export interface CategoryStore {
  customCategories: FindingCategory[];
  categoryOrder: string[]; // All category IDs in display order
  hiddenCategories: string[]; // Categories to hide from display
}
