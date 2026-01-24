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

// Session Management Services
export { SessionManager, sessionManager } from "./SessionManager";
export type { SessionContext, SessionStatus, SessionChangeListener } from "./SessionManager";

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

// AI Data Service (Task runs, logs, screenshots for AI Data Viewer)
export { aiDataService } from "./ai-data-service";

// DOM Capture Service (browser DOM snapshot capture for AI debugging)
export { domCaptureService } from "./dom-capture-service";

// Learning Service (Learning Insights Dashboard)
export { learningService } from "./learning-service";

// Flow Service (Flow Designer for deterministic workflows)
export { flowService } from "./flow-service";

// Checkpoint Service (Checkpoint Browser for time-travel debugging)
export { checkpointService } from "./checkpoint-service";

// Workflow Generator Service (State Machine → Unified Workflow conversion)
export {
  WorkflowGeneratorService,
  workflowGeneratorService,
} from "./WorkflowGeneratorService";
export type {
  GeneratorState,
  GeneratorStateImage,
  GeneratorStateRegion,
  GeneratorStateLocation,
  GeneratorStateString,
  GeneratorContext,
  GeneratorConfig,
  GeneratorOptions,
  SingleStateGeneratorOptions,
  SingleStateAgenticOptions,
} from "./WorkflowGeneratorService";
