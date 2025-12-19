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

// Verification Services (AI self-healing)
export { verificationService } from "./VerificationService";
export type {
  PendingVerification,
  VerificationPendingMarker,
  VerificationCompletedMarker,
  VerificationFailedMarker,
  RunnerRestartMarker,
} from "./VerificationService";
