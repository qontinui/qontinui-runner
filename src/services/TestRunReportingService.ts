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
 */

import { invoke } from "@tauri-apps/api/core";

// ============================================================================
// Types
// ============================================================================

/** Input for creating a test run */
interface CreateTestRunInput {
  project_id: string;
  workflow_id: string;
  workflow_name: string;
  total_states?: number;
  total_transitions?: number;
  description?: string;
}

/** Response from creating a test run */
interface CreateTestRunResponse {
  run_id: string;
  project_id: string;
  run_name: string;
  status: string;
  started_at: string;
  ended_at?: string;
  duration_seconds?: number;
}

/** Transition data for reporting - matches backend TransitionCreate schema */
export interface TransitionData {
  sequence_number: number;
  from_state: string;
  to_state: string;
  transition_name: string;
  status: "success" | "failed" | "skipped" | "timeout";
  started_at: string;
  completed_at: string;
  duration_ms: number;
  error_message?: string;
  error_type?: string;
  screenshot_id?: string;
  metadata?: Record<string, unknown>;
}

/** Response from reporting transitions */
interface ReportTransitionsResponse {
  recorded: number;
  run_id: string;
}

/** Image recognition data for historical storage */
export interface ImageRecognitionData {
  pattern_id: string;
  pattern_name?: string;
  action_type: string;
  active_states: string[];
  success: boolean;
  match_count?: number;
  best_match_score?: number;
  match_x?: number;
  match_y?: number;
  match_width?: number;
  match_height?: number;
  duration_ms?: number;
  result_data?: Record<string, unknown>;
}

/** Input for reporting image recognition batch */
interface ReportRecognitionsInput {
  run_id: string;
  project_id: string;
  workflow_id?: number;
  test_run_id: string;
  actions: Array<ImageRecognitionData & { action_id: string; sequence_number: number }>;
}

/** Response from reporting recognitions */
interface ReportRecognitionsResponse {
  indexed: number;
  run_id: string;
}

/** Input for completing a test run - matches Rust CompleteTestRunInput */
interface CompleteTestRunInput {
  run_id: string;
  success: boolean;
  total_transitions: number;
  successful_transitions: number;
  failed_transitions: number;
  timeout_transitions?: number;
  unique_transitions_covered?: number;
  coverage_percentage?: number;
  states_covered?: number;
  total_states?: number;
  deficiencies_found?: number;
  screenshots_captured?: number;
  duration_seconds?: number;
}

/** Response from completing a test run */
interface CompleteTestRunResponse {
  run_id: string;
  status: string;
  duration_seconds: number;
}

/** Execution stats tracked during a run */
interface ExecutionStats {
  totalTransitions: number;
  successfulTransitions: number;
  failedTransitions: number;
  statesCovered: Set<string>;
  totalRecognitions: number;
  successfulRecognitions: number;
  failedRecognitions: number;
}

// ============================================================================
// Service Implementation
// ============================================================================

/**
 * @deprecated Use ExecutionReportingService instead.
 * This class is kept for backward compatibility with existing /api/v1/testing/* endpoints.
 */
class TestRunReportingServiceImpl {
  private activeRunId: string | null = null;
  private activeProjectId: string | null = null;
  private activeWorkflowId: number | null = null;
  private pendingTransitions: TransitionData[] = [];
  private pendingRecognitions: Array<ImageRecognitionData & { action_id: string; sequence_number: number }> = [];
  private transitionSequenceNumber = 0;
  private recognitionSequenceNumber = 0;
  private flushTimeout: ReturnType<typeof setTimeout> | null = null;
  private recognitionFlushTimeout: ReturnType<typeof setTimeout> | null = null;
  private executionStats: ExecutionStats = {
    totalTransitions: 0,
    successfulTransitions: 0,
    failedTransitions: 0,
    statesCovered: new Set(),
    totalRecognitions: 0,
    successfulRecognitions: 0,
    failedRecognitions: 0,
  };

  // Flush transitions every 5 seconds or when batch reaches 10
  private readonly FLUSH_INTERVAL_MS = 5000;
  private readonly BATCH_SIZE = 10;

  /**
   * Start a new test run for the given workflow
   */
  async startTestRun(
    projectId: string,
    workflowId: string,
    workflowName: string,
    options?: {
      totalStates?: number;
      totalTransitions?: number;
      description?: string;
    }
  ): Promise<string | null> {
    if (!projectId) {
      console.warn("[TestRunReporting] No project ID provided, skipping test run creation");
      return null;
    }

    try {
      console.log(`[TestRunReporting] Starting test run for workflow: ${workflowName}`);

      const input: CreateTestRunInput = {
        project_id: projectId,
        workflow_id: workflowId,
        workflow_name: workflowName,
        total_states: options?.totalStates ?? 0,
        total_transitions: options?.totalTransitions ?? 0,
        description: options?.description,
      };

      const response = await invoke<CreateTestRunResponse>("create_test_run", { input });

      this.activeRunId = response.run_id;
      this.activeProjectId = projectId;
      this.activeWorkflowId = parseInt(workflowId, 10) || null;
      this.resetStats();

      console.log(`[TestRunReporting] Test run created: ${response.run_id}`);
      return response.run_id;
    } catch (error) {
      console.error("[TestRunReporting] Failed to create test run:", error);
      return null;
    }
  }

  /**
   * Report a transition that occurred during execution
   */
  async reportTransition(transition: TransitionData): Promise<void> {
    if (!this.activeRunId) {
      // No active test run, skip reporting
      return;
    }

    // Update stats
    this.executionStats.totalTransitions++;
    if (transition.status === "success") {
      this.executionStats.successfulTransitions++;
    } else if (transition.status === "failed") {
      this.executionStats.failedTransitions++;
    }

    // Track states covered (using the new field names)
    if (transition.from_state) {
      this.executionStats.statesCovered.add(transition.from_state);
    }
    if (transition.to_state) {
      this.executionStats.statesCovered.add(transition.to_state);
    }

    // Add to pending batch
    this.pendingTransitions.push(transition);

    // Flush if batch is full
    if (this.pendingTransitions.length >= this.BATCH_SIZE) {
      await this.flushTransitions();
    } else {
      // Schedule flush if not already scheduled
      this.scheduleFlush();
    }
  }

  /**
   * Report an image recognition result for historical storage
   */
  async reportImageRecognition(data: ImageRecognitionData): Promise<void> {
    if (!this.activeRunId || !this.activeProjectId) {
      // No active test run, skip reporting
      return;
    }

    // Update stats
    this.executionStats.totalRecognitions++;
    if (data.success) {
      this.executionStats.successfulRecognitions++;
    } else {
      this.executionStats.failedRecognitions++;
    }

    // Track states covered from active_states
    for (const state of data.active_states) {
      this.executionStats.statesCovered.add(state);
    }

    // Increment sequence number
    this.recognitionSequenceNumber++;

    // Add to pending batch with sequence number
    this.pendingRecognitions.push({
      ...data,
      action_id: `recognition-${Date.now()}-${this.recognitionSequenceNumber}`,
      sequence_number: this.recognitionSequenceNumber,
    });

    // Flush if batch is full
    if (this.pendingRecognitions.length >= this.BATCH_SIZE) {
      await this.flushRecognitions();
    } else {
      // Schedule flush if not already scheduled
      this.scheduleRecognitionFlush();
    }
  }

  /**
   * Complete the active test run with final status
   */
  async completeTestRun(success: boolean): Promise<void> {
    if (!this.activeRunId) {
      console.warn("[TestRunReporting] No active test run to complete");
      return;
    }

    try {
      // Flush any remaining transitions and recognitions
      await this.flushTransitions();
      await this.flushRecognitions();

      console.log(
        `[TestRunReporting] Completing test run ${this.activeRunId} with status: ${success ? "completed" : "failed"}`
      );

      const input: CompleteTestRunInput = {
        run_id: this.activeRunId,
        success,
        total_transitions: this.executionStats.totalTransitions,
        successful_transitions: this.executionStats.successfulTransitions,
        failed_transitions: this.executionStats.failedTransitions,
        timeout_transitions: 0,
        unique_transitions_covered: this.executionStats.statesCovered.size,
        states_covered: this.executionStats.statesCovered.size,
        coverage_percentage: 0, // Would need total states to calculate
        total_states: 0,
        deficiencies_found: 0,
        screenshots_captured: 0,
        duration_seconds: 0, // Will be calculated by backend from started_at/ended_at
      };

      const response = await invoke<CompleteTestRunResponse>("complete_test_run", { input });

      console.log(
        `[TestRunReporting] Test run completed: ${response.status}, duration: ${response.duration_seconds}s`
      );

      // Reset state
      this.activeRunId = null;
      this.activeProjectId = null;
      this.activeWorkflowId = null;
      this.resetStats();
    } catch (error) {
      console.error("[TestRunReporting] Failed to complete test run:", error);
      // Reset state even on error
      this.activeRunId = null;
      this.activeProjectId = null;
      this.activeWorkflowId = null;
      this.resetStats();
    }
  }

  /**
   * Check if there's an active test run
   */
  get isActive(): boolean {
    return this.activeRunId !== null;
  }

  /**
   * Get the current run ID
   */
  get currentRunId(): string | null {
    return this.activeRunId;
  }

  /**
   * Get current execution stats
   */
  get stats(): Readonly<{
    totalTransitions: number;
    successfulTransitions: number;
    failedTransitions: number;
    statesCovered: number;
  }> {
    return {
      totalTransitions: this.executionStats.totalTransitions,
      successfulTransitions: this.executionStats.successfulTransitions,
      failedTransitions: this.executionStats.failedTransitions,
      statesCovered: this.executionStats.statesCovered.size,
    };
  }

  /**
   * Get the next sequence number for a transition.
   * This should be called before creating TransitionData to ensure unique sequence numbers.
   */
  getNextTransitionSequenceNumber(): number {
    return ++this.transitionSequenceNumber;
  }

  // ============================================================================
  // Private Methods
  // ============================================================================

  private resetStats(): void {
    this.executionStats = {
      totalTransitions: 0,
      successfulTransitions: 0,
      failedTransitions: 0,
      statesCovered: new Set(),
      totalRecognitions: 0,
      successfulRecognitions: 0,
      failedRecognitions: 0,
    };
    this.pendingTransitions = [];
    this.pendingRecognitions = [];
    this.transitionSequenceNumber = 0;
    this.recognitionSequenceNumber = 0;
    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }
    if (this.recognitionFlushTimeout) {
      clearTimeout(this.recognitionFlushTimeout);
      this.recognitionFlushTimeout = null;
    }
  }

  private scheduleFlush(): void {
    if (this.flushTimeout) {
      return; // Already scheduled
    }

    this.flushTimeout = setTimeout(() => {
      this.flushTimeout = null;
      this.flushTransitions().catch((error) => {
        console.error("[TestRunReporting] Failed to flush transitions:", error);
      });
    }, this.FLUSH_INTERVAL_MS);
  }

  private async flushTransitions(): Promise<void> {
    if (!this.activeRunId || this.pendingTransitions.length === 0) {
      return;
    }

    const transitions = [...this.pendingTransitions];
    this.pendingTransitions = [];

    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }

    try {
      console.log(
        `[TestRunReporting] Reporting ${transitions.length} transitions for run ${this.activeRunId}`
      );

      const response = await invoke<ReportTransitionsResponse>("report_test_transitions", {
        runId: this.activeRunId,
        transitions,
      });

      console.log(`[TestRunReporting] Transitions reported: ${response.recorded} recorded`);
    } catch (error) {
      console.error("[TestRunReporting] Failed to report transitions:", error);
      // Re-add transitions to pending on failure (will be retried on next flush)
      this.pendingTransitions = [...transitions, ...this.pendingTransitions];
    }
  }

  private scheduleRecognitionFlush(): void {
    if (this.recognitionFlushTimeout) {
      return; // Already scheduled
    }

    this.recognitionFlushTimeout = setTimeout(() => {
      this.recognitionFlushTimeout = null;
      this.flushRecognitions().catch((error) => {
        console.error("[TestRunReporting] Failed to flush recognitions:", error);
      });
    }, this.FLUSH_INTERVAL_MS);
  }

  private async flushRecognitions(): Promise<void> {
    if (!this.activeRunId || !this.activeProjectId || this.pendingRecognitions.length === 0) {
      return;
    }

    const recognitions = [...this.pendingRecognitions];
    this.pendingRecognitions = [];

    if (this.recognitionFlushTimeout) {
      clearTimeout(this.recognitionFlushTimeout);
      this.recognitionFlushTimeout = null;
    }

    try {
      console.log(
        `[TestRunReporting] Reporting ${recognitions.length} recognitions for run ${this.activeRunId}`
      );

      const input: ReportRecognitionsInput = {
        run_id: this.activeRunId,
        project_id: this.activeProjectId,
        workflow_id: this.activeWorkflowId ?? undefined,
        test_run_id: this.activeRunId,
        actions: recognitions,
      };

      const response = await invoke<ReportRecognitionsResponse>("report_image_recognitions", {
        input,
      });

      console.log(`[TestRunReporting] Recognitions reported: ${response.indexed} indexed`);
    } catch (error) {
      console.error("[TestRunReporting] Failed to report recognitions:", error);
      // Re-add recognitions to pending on failure (will be retried on next flush)
      this.pendingRecognitions = [...recognitions, ...this.pendingRecognitions];
    }
  }
}

// Export singleton instance
/** @deprecated Use executionReportingService from ExecutionReportingService instead */
export const testRunReportingService = new TestRunReportingServiceImpl();

// Export helper functions for direct use
/** @deprecated Use startRun from ExecutionReportingService instead */
export const startTestRun = testRunReportingService.startTestRun.bind(testRunReportingService);
/** @deprecated Use reportAction/reportActions from ExecutionReportingService instead */
export const reportTransition = testRunReportingService.reportTransition.bind(testRunReportingService);
/** @deprecated Use reportAction/reportActions from ExecutionReportingService instead */
export const reportImageRecognition = testRunReportingService.reportImageRecognition.bind(testRunReportingService);
/** @deprecated Use getNextActionSequenceNumber from ExecutionReportingService instead */
export const getNextTransitionSequenceNumber = testRunReportingService.getNextTransitionSequenceNumber.bind(testRunReportingService);
/** @deprecated Use completeRun from ExecutionReportingService instead */
export const completeTestRun = testRunReportingService.completeTestRun.bind(testRunReportingService);
