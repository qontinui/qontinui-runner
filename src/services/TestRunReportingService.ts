/**
 * TestRunReportingService
 *
 * @deprecated This service is deprecated in favor of ExecutionReportingService.
 * Use ExecutionReportingService for new implementations which provides a unified
 * schema supporting multiple run types: QA testing, integration testing,
 * live automation, recording sessions, and debug runs.
 *
 * This service remains for backward compatibility with the existing /api/v1/testing/*
 * endpoints. It will be removed in a future version once the backend fully supports
 * the unified /api/v1/execution/* endpoints.
 *
 * Service for reporting test run execution to the qontinui-web backend QA Dashboard.
 * This enables workflow executions to be tracked and analyzed in the web UI.
 *
 * REFACTORED: The implementation has been split into focused modules:
 * - test-run-reporting/types.ts - Shared interfaces and types
 * - test-run-reporting/TestRunCreation.ts - Creating new test runs
 * - test-run-reporting/TestRunStats.ts - Tracking execution statistics
 * - test-run-reporting/TestRunPersistence.ts - Batching and flushing data
 * - test-run-reporting/TestRunCompletion.ts - Completing test runs
 * - test-run-reporting/TestRunReporting.ts - Main coordinator service
 *
 * This file re-exports everything for backward compatibility.
 */

// Re-export types
export type { TransitionData, ImageRecognitionData } from "./test-run-reporting";

// Re-export the singleton and helper functions
export {
  testRunReportingService,
  startTestRun,
  reportTransition,
  reportImageRecognition,
  getNextTransitionSequenceNumber,
  completeTestRunHelper as completeTestRun,
} from "./test-run-reporting";
