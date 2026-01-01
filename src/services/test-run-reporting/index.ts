/**
 * Test Run Reporting Module
 *
 * This module provides functionality for reporting test run execution
 * to the qontinui-web backend QA Dashboard.
 *
 * @deprecated This module is deprecated in favor of ExecutionReportingService.
 * Use ExecutionReportingService for new implementations which provides a unified
 * schema supporting multiple run types: QA testing, integration testing,
 * live automation, recording sessions, and debug runs.
 *
 * This module remains for backward compatibility with the existing /api/v1/testing/*
 * endpoints. It will be removed in a future version once the backend fully supports
 * the unified /api/v1/execution/* endpoints.
 */

// ============================================================================
// Types
// ============================================================================

export type {
  // Input types
  CreateTestRunInput,
  CompleteTestRunInput,
  ReportRecognitionsInput,
  // Response types
  CreateTestRunResponse,
  ReportTransitionsResponse,
  ReportRecognitionsResponse,
  CompleteTestRunResponse,
  // Data types
  TransitionData,
  ImageRecognitionData,
  SequencedRecognitionData,
  // Internal state types
  ExecutionStats,
  ActiveRunState,
  BatchConfig,
  PublicExecutionStats,
} from "./types";

export { DEFAULT_BATCH_CONFIG } from "./types";

// ============================================================================
// Creation
// ============================================================================

export {
  createTestRun,
  parseWorkflowId,
  createActiveRunState,
  createEmptyRunState,
  type StartTestRunOptions,
} from "./TestRunCreation";

// ============================================================================
// Stats
// ============================================================================

export {
  createExecutionStats,
  updateStatsForTransition,
  updateStatsForRecognition,
  toPublicStats,
  getStatesCoveredCount,
} from "./TestRunStats";

// ============================================================================
// Persistence (Batching)
// ============================================================================

export {
  TransitionBatcher,
  RecognitionBatcher,
  flushTransitions,
  flushRecognitions,
} from "./TestRunPersistence";

// ============================================================================
// Completion
// ============================================================================

export { completeTestRun } from "./TestRunCompletion";

// ============================================================================
// Main Service
// ============================================================================

export { TestRunReportingServiceImpl } from "./TestRunReporting";

// ============================================================================
// Singleton Instance (for backward compatibility)
// ============================================================================

import { TestRunReportingServiceImpl } from "./TestRunReporting";

/**
 * Singleton instance of the test run reporting service
 * @deprecated Use executionReportingService from ExecutionReportingService instead
 */
export const testRunReportingService = new TestRunReportingServiceImpl();

// ============================================================================
// Helper Functions (for backward compatibility)
// ============================================================================

/** @deprecated Use startRun from ExecutionReportingService instead */
export const startTestRun = testRunReportingService.startTestRun.bind(testRunReportingService);

/** @deprecated Use reportAction/reportActions from ExecutionReportingService instead */
export const reportTransition =
  testRunReportingService.reportTransition.bind(testRunReportingService);

/** @deprecated Use reportAction/reportActions from ExecutionReportingService instead */
export const reportImageRecognition =
  testRunReportingService.reportImageRecognition.bind(testRunReportingService);

/** @deprecated Use getNextActionSequenceNumber from ExecutionReportingService instead */
export const getNextTransitionSequenceNumber =
  testRunReportingService.getNextTransitionSequenceNumber.bind(testRunReportingService);

/** @deprecated Use completeRun from ExecutionReportingService instead */
export const completeTestRunHelper =
  testRunReportingService.completeTestRun.bind(testRunReportingService);
