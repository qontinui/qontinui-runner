/**
 * Services Barrel Export
 *
 * Central export point for all TypeScript services.
 */

// Configuration Services
export { ConfigurationParserService, configurationParser } from "./ConfigurationParserService";
export type {
  Config,
  ConfigImage,
  ConfigState,
  ConfigRawData,
  FilterStats,
  ParsedConfig,
  Workflow,
} from "./ConfigurationParserService";

export { ConfigurationLoaderService, configurationLoader } from "./ConfigurationLoaderService";
export type {
  LoadingSource,
  LoadResult,
  ConfigLoadedEventPayload,
} from "./ConfigurationLoaderService";

// Storage Services
export { LocalStorageService } from "./LocalStorageService";
export type { StorageConfig, StorageUsage, CommandResponse } from "./LocalStorageService";

// Config Storage Services (HTTP API for stored configs)
export { ConfigStorageService } from "./ConfigStorageService";
export type { StoredConfigMetadata, StoredConfig } from "./ConfigStorageService";

// Video Recording Services
export { VideoRecordingService } from "./VideoRecordingService";

// State Detection Services
export { StateDetectionService } from "./StateDetectionService";

// Issue Tracking Services
export { IssueTracker, issueTracker } from "./IssueTracker";
export type { IssueTrackerEventType, IssueTrackerEvent } from "./IssueTracker";

// Session Management Services
export { SessionManager, sessionManager } from "./SessionManager";
export type { SessionContext, SessionStatus, SessionChangeListener } from "./SessionManager";

// Issue Sync Services (runner → web backend)
export {
  syncIssuesToBackend,
  syncSessionIssues,
  syncSpecificIssues,
  issueSyncService,
} from "./IssueSyncService";
export type { SyncIssuesResponse } from "./IssueSyncService";

// Verification Services (AI self-healing)
export { verificationService } from "./VerificationService";
export type {
  PendingVerification,
  VerificationPendingMarker,
  VerificationCompletedMarker,
  VerificationFailedMarker,
  RunnerRestartMarker,
} from "./VerificationService";

// Findings Tracking Services (categorized findings system)
export { FindingsTracker, findingsTracker } from "./FindingsTracker";
export type { FindingsTrackerEventType, FindingsTrackerEvent } from "./FindingsTracker";

// Finding Categories Service
export {
  BUILT_IN_CATEGORIES,
  getAllCategories,
  getVisibleCategories,
  getCategoryById,
  isBuiltInCategory,
  addCustomCategory,
  updateCustomCategory,
  deleteCustomCategory,
  setCategoryVisibility,
  setCategoryOrder,
  resetCategories,
  getCategoryColorClasses,
} from "./FindingCategories";

// Unified Execution Reporting Services (new unified schema)
export {
  executionReportingService,
  startRun,
  reportAction,
  reportActions,
  reportScreenshot,
  reportIssues,
  completeRun,
  getNextActionSequenceNumber,
} from "./ExecutionReportingService";
export type {
  RunnerMetadata,
  WorkflowMetadata,
  ExecutionStats,
  CoverageData,
  ActionExecutionCreate,
  ExecutionScreenshotCreate,
  ExecutionIssueCreate,
} from "./ExecutionReportingService";

// Report Persistence Services (unified local + backend persistence)
export { ReportPersistenceService, reportPersistenceService } from "./ReportPersistenceService";
export type { BackendConfig, SyncResult, PendingSyncItem } from "./ReportPersistenceService";

// Unified Report Service (aggregates findings + issues)
export { UnifiedReportService, unifiedReportService } from "./UnifiedReportService";
export type {
  UnifiedReportItem,
  UnifiedSeverity,
  UnifiedStatus,
  UnifiedReportSummary,
  UnifiedReportEventType,
  UnifiedReportEvent,
} from "./UnifiedReportService";

// Statistics Service (Tiered Information Model dashboard)
export {
  statisticsService,
  isSuccess,
  unwrapResponse,
  calculateSuccessRate,
  formatDuration,
  getAnomalySeverityColor,
  getRunStatusColor,
} from "./statistics-service";

// Discoveries Service (Discovery Push mechanism)
export { discoveriesService } from "./discoveries-service";
