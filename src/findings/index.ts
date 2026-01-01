/**
 * Findings Module
 *
 * Comprehensive service for tracking categorized findings from AI analysis.
 * This module exports all findings-related functionality.
 */

// Re-export types
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
  FindingsTrackerEventType,
  FindingsTrackerEvent,
  FindingsTrackerListener,
  SessionHistoryEntry,
  PersistedFindingsData,
  FindingParsingMeta,
} from "./types";

// Persistence module
export { persistFindingsData, loadFindingsData, archiveSession } from "./FindingsPersistence";

// Query module
export {
  getAllFindings,
  getSessionFindings,
  getFindingsByCategory,
  getFindingsByStatus,
  getFindingsBySeverity,
  getFindingsNeedingInput,
  getActionableFindings,
  getUnresolvedCount,
  filterFindings,
  sortFindings,
  groupFindings,
} from "./FindingsQuery";

// Export module
export {
  createEmptySummary,
  calculateSummary,
  exportToJson,
  exportToMarkdown,
  exportToCsv,
  exportFindings,
} from "./FindingsExport";
export type { ExportFormat, ExportOptions } from "./FindingsExport";

// Sync module
export {
  createReport,
  updateReportWithFindings,
  completeReport,
  startNewPhase,
  syncFindingsToBackend,
  syncReportToBackend,
  checkBackendHealth,
} from "./FindingsSync";
export type { BackendSyncOptions, SyncResult } from "./FindingsSync";
