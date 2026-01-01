/**
 * Findings Module Types
 *
 * Shared types for the findings tracking system.
 * Re-exports types from the main types/findings.ts and adds module-specific types.
 */

// Re-export all types from the main findings types
export type {
  Finding,
  FindingSeverity,
  FindingStatus,
  ActionType,
  UserInputRequest,
  ExecutionReport,
  ReportSummary,
  PhaseInfo,
  ParsedFinding,
  CodeContext,
  FindingCategory,
  CategorySummary,
  ReportStatus,
  UserInputOption,
  UserInputType,
  BuiltInCategoryId,
  CategoryStore,
} from "../types/findings";

/** Event types emitted by FindingsTracker */
export type FindingsTrackerEventType =
  | "finding_detected"
  | "finding_updated"
  | "finding_resolved"
  | "finding_removed"
  | "findings_cleared"
  | "report_created"
  | "report_updated"
  | "input_requested"
  | "input_received";

import type { Finding, ExecutionReport, UserInputRequest } from "../types/findings";

/** Event payload for findings tracker events */
export interface FindingsTrackerEvent {
  type: FindingsTrackerEventType;
  finding?: Finding;
  findings?: Finding[];
  report?: ExecutionReport;
  inputRequest?: UserInputRequest;
}

/** Listener callback type for findings events */
export type FindingsTrackerListener = (event: FindingsTrackerEvent) => void;

/**
 * Session history entry - a completed session with its findings
 */
export interface SessionHistoryEntry {
  id: string;
  sessionId: string;
  promptName: string;
  startedAt: number;
  completedAt: number;
  duration: number;
  status: "completed" | "failed" | "timeout" | "context_limit";
  phases: number;
  findings: Finding[];
  summary: {
    total: number;
    resolved: number;
    pending: number;
    bySeverity: Record<string, number>;
    byCategory: Record<string, number>;
  };
}

/**
 * Data structure for persisted findings
 */
export interface PersistedFindingsData {
  version: number;
  savedAt: number;
  currentSessionId: string;
  findings: Finding[];
  reports: ExecutionReport[];
  sessionHistory: SessionHistoryEntry[];
}

/**
 * Metadata for finding parsing state
 */
export interface FindingParsingMeta {
  categoryId: string;
  severity: import("../types/findings").FindingSeverity;
  needsInput: boolean;
}
