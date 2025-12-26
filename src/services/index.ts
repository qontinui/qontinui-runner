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

// Video Recording Services
export { VideoRecordingService } from "./VideoRecordingService";

// State Detection Services
export { StateDetectionService } from "./StateDetectionService";

// Issue Tracking Services
export { IssueTracker, issueTracker } from "./IssueTracker";
export type { IssueTrackerEventType, IssueTrackerEvent } from "./IssueTracker";

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

// Legacy Test Run Reporting Services (deprecated - use ExecutionReportingService)
// @deprecated Use ExecutionReportingService instead
export {
  testRunReportingService,
  startTestRun,
  reportTransition,
  completeTestRun,
} from "./TestRunReportingService";
export type { TransitionData } from "./TestRunReportingService";
