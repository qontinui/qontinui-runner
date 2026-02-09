/**
 * ExecutionReportingService
 *
 * Unified service for reporting execution runs to the qontinui-web backend.
 * Supports multiple run types: QA testing, integration testing, live automation,
 * recording sessions, and debug runs.
 *
 * This service replaces TestRunReportingService with a more flexible, unified schema.
 */

import { invoke } from "@tauri-apps/api/core";
import { ActionStatus, ActionType } from "../types/execution";
import type {
  RunType,
  RunStatus,
  RunnerMetadata,
  WorkflowMetadata,
  ExecutionStats,
  CoverageData,
  ExecutionRunCreate,
  ExecutionRunResponse,
  ActionExecutionCreate,
  ActionExecutionResponse,
  ExecutionScreenshotCreate,
  ExecutionScreenshotResponse,
  ExecutionIssueCreate,
  ExecutionIssueResponse,
  ExecutionRunComplete,
  ExecutionRunCompleteResponse,
} from "../types/execution";

// Re-export types for convenience
export type {
  RunType,
  RunStatus,
  ActionStatus,
  ActionType,
  RunnerMetadata,
  WorkflowMetadata,
  ExecutionStats,
  CoverageData,
  ActionExecutionCreate,
  ExecutionScreenshotCreate,
  ExecutionIssueCreate,
};

// ============================================================================
// Legacy Compatibility Types
// ============================================================================

/**
 * Legacy transition data format for backward compatibility.
 * Maps to ActionExecutionCreate with action_type = TRANSITION.
 */
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

/**
 * Legacy image recognition data format for backward compatibility.
 * Maps to ActionExecutionCreate with action_type = FIND.
 */
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

// ============================================================================
// Internal Types
// ============================================================================

/** Internal execution stats tracked during a run */
interface InternalExecutionStats {
  totalActions: number;
  successfulActions: number;
  failedActions: number;
  timeoutActions: number;
  skippedActions: number;
  statesCovered: Set<string>;
  transitionsCovered: Set<string>;
  startTime: number;
  // Recognition-specific stats (for backward compatibility with legacy service)
  totalRecognitions: number;
  successfulRecognitions: number;
  failedRecognitions: number;
}

// ============================================================================
// Service Implementation
// ============================================================================

class ExecutionReportingServiceImpl {
  private activeRunId: string | null = null;
  private activeProjectId: string | null = null;
  private activeRunType: RunType | null = null;
  private pendingActions: ActionExecutionCreate[] = [];
  private actionSequenceNumber = 0;
  private flushTimeout: ReturnType<typeof setTimeout> | null = null;
  private executionStats: InternalExecutionStats = this.createEmptyStats();

  // Flush actions every 5 seconds or when batch reaches 20
  private readonly FLUSH_INTERVAL_MS = 5000;
  private readonly BATCH_SIZE = 20;

  // ============================================================================
  // Public API
  // ============================================================================

  /**
   * Start a new execution run.
   *
   * @param projectId - Project ID
   * @param runType - Type of run (qa_test, integration_test, live_automation, recording, debug)
   * @param runName - Human-readable name for the run
   * @param runnerMetadata - Metadata about the runner environment
   * @param workflowMetadata - Optional metadata about the workflow being executed
   * @param configuration - Optional configuration snapshot
   * @returns The run ID if successful, null otherwise
   */
  async startRun(
    projectId: string,
    runType: RunType,
    runName: string,
    runnerMetadata: RunnerMetadata,
    workflowMetadata?: WorkflowMetadata,
    configuration?: Record<string, unknown>,
  ): Promise<string | null> {
    if (!projectId) {
      console.warn("[ExecutionReporting] No project ID provided, skipping run creation");
      return null;
    }

    try {
      console.log(`[ExecutionReporting] Starting ${runType} run: ${runName}`);
      console.log(
        `[ExecutionReporting] workflowMetadata: ${JSON.stringify(workflowMetadata, null, 2)}`,
      );

      const input: ExecutionRunCreate = {
        project_id: projectId,
        run_type: runType,
        run_name: runName,
        runner_metadata: runnerMetadata,
        workflow_metadata: workflowMetadata,
        configuration,
      };

      const response = await invoke<ExecutionRunResponse>("create_execution_run", { input });

      this.activeRunId = response.run_id;
      this.activeProjectId = projectId;
      this.activeRunType = runType;
      this.resetStats();

      console.log(`[ExecutionReporting] Run created: ${response.run_id}`);
      return response.run_id;
    } catch (error) {
      console.error("[ExecutionReporting] Failed to create run:", error);
      return null;
    }
  }

  /**
   * Report action executions that occurred during the run.
   * Actions are batched and flushed periodically or when the batch is full.
   *
   * @param actions - Array of action executions to report
   */
  async reportActions(actions: ActionExecutionCreate[]): Promise<void> {
    if (!this.activeRunId || actions.length === 0) {
      return;
    }

    // Update stats
    for (const action of actions) {
      this.updateStatsFromAction(action);
    }

    // Add to pending batch
    this.pendingActions.push(...actions);

    // Flush if batch is full
    if (this.pendingActions.length >= this.BATCH_SIZE) {
      await this.flushActions();
    } else {
      this.scheduleFlush();
    }
  }

  /**
   * Report a single action execution.
   * This is a convenience method that calls reportActions with a single action.
   *
   * @param action - The action execution to report
   */
  async reportAction(action: ActionExecutionCreate): Promise<void> {
    await this.reportActions([action]);
  }

  /**
   * Upload a screenshot for the current run.
   *
   * @param screenshot - Screenshot metadata
   * @param imageData - Raw image data (PNG or JPEG)
   * @returns Screenshot response if successful
   */
  async reportScreenshot(
    screenshot: ExecutionScreenshotCreate,
    imageData: Uint8Array,
  ): Promise<ExecutionScreenshotResponse | null> {
    if (!this.activeRunId) {
      console.warn("[ExecutionReporting] No active run for screenshot upload");
      return null;
    }

    try {
      console.log(
        `[ExecutionReporting] Uploading screenshot ${screenshot.screenshot_id} for run ${this.activeRunId}`,
      );

      const response = await invoke<ExecutionScreenshotResponse>("upload_execution_screenshot", {
        runId: this.activeRunId,
        screenshot,
        imageData: Array.from(imageData),
      });

      console.log(`[ExecutionReporting] Screenshot uploaded: ${response.image_url}`);
      return response;
    } catch (error) {
      console.error("[ExecutionReporting] Failed to upload screenshot:", error);
      return null;
    }
  }

  /**
   * Report issues discovered during the run.
   *
   * @param issues - Array of issues to report
   */
  async reportIssues(issues: ExecutionIssueCreate[]): Promise<void> {
    if (!this.activeRunId || issues.length === 0) {
      return;
    }

    try {
      console.log(
        `[ExecutionReporting] Reporting ${issues.length} issues for run ${this.activeRunId}`,
      );

      const response = await invoke<ExecutionIssueResponse>("report_execution_issues", {
        runId: this.activeRunId,
        issues,
      });

      console.log(`[ExecutionReporting] Issues reported: ${response.recorded} recorded`);
    } catch (error) {
      console.error("[ExecutionReporting] Failed to report issues:", error);
    }
  }

  // ============================================================================
  // Legacy Compatibility Methods
  // ============================================================================

  /**
   * Report an image recognition result.
   *
   * This method provides backward compatibility with TestRunReportingService.
   * It converts ImageRecognitionData to ActionExecutionCreate with action_type = FIND.
   *
   * @param recognition - The image recognition data to report
   */
  async reportImageRecognition(recognition: ImageRecognitionData): Promise<void> {
    if (!this.activeRunId) {
      console.warn("[ExecutionReporting] No active run for image recognition report");
      return;
    }

    // Update recognition-specific stats
    this.executionStats.totalRecognitions++;
    if (recognition.success) {
      this.executionStats.successfulRecognitions++;
    } else {
      this.executionStats.failedRecognitions++;
    }

    // Track states covered from active_states
    for (const state of recognition.active_states) {
      this.executionStats.statesCovered.add(state);
    }

    // Map ImageRecognitionData to ActionExecutionCreate
    const action: ActionExecutionCreate = {
      sequence_number: this.getNextActionSequenceNumber(),
      action_type: ActionType.FIND,
      action_name: recognition.pattern_name || recognition.pattern_id,
      status: recognition.success ? ActionStatus.SUCCESS : ActionStatus.FAILED,
      started_at: new Date().toISOString(),
      completed_at: new Date().toISOString(),
      duration_ms: recognition.duration_ms || 0,
      pattern_id: recognition.pattern_id,
      pattern_name: recognition.pattern_name,
      active_states: recognition.active_states,
      confidence_score: recognition.best_match_score,
      match_location:
        recognition.match_x !== undefined
          ? {
              x: recognition.match_x,
              y: recognition.match_y!,
              width: recognition.match_width,
              height: recognition.match_height,
            }
          : undefined,
      output_data: {
        match_count: recognition.match_count,
        ...recognition.result_data,
      },
    };

    await this.reportAction(action);
  }

  /**
   * Report a transition that occurred during the run.
   *
   * This method provides backward compatibility with TestRunReportingService.
   * It converts TransitionData to ActionExecutionCreate with action_type = TRANSITION.
   *
   * @param transition - The transition data to report
   */
  async reportTransition(transition: TransitionData): Promise<void> {
    if (!this.activeRunId) {
      console.warn("[ExecutionReporting] No active run for transition report");
      return;
    }

    // Map status string to ActionStatus enum
    const statusMap: Record<TransitionData["status"], ActionStatus> = {
      success: ActionStatus.SUCCESS,
      failed: ActionStatus.FAILED,
      skipped: ActionStatus.SKIPPED,
      timeout: ActionStatus.TIMEOUT,
    };

    // Map TransitionData to ActionExecutionCreate
    const action: ActionExecutionCreate = {
      sequence_number: transition.sequence_number,
      action_type: ActionType.TRANSITION,
      action_name: transition.transition_name,
      status: statusMap[transition.status],
      started_at: transition.started_at,
      completed_at: transition.completed_at,
      duration_ms: transition.duration_ms,
      from_state: transition.from_state,
      to_state: transition.to_state,
      error_message: transition.error_message,
      screenshot_id: transition.screenshot_id,
      metadata: transition.metadata,
    };

    await this.reportAction(action);
  }

  /**
   * Complete the active execution run with final status and statistics.
   *
   * @param status - Final status (completed, failed, timeout, cancelled)
   * @param stats - Optional execution stats (will use tracked stats if not provided)
   * @param coverage - Optional coverage data
   * @param errorMessage - Optional error message if run failed
   */
  async completeRun(
    status: RunStatus,
    stats?: ExecutionStats,
    coverage?: CoverageData,
    errorMessage?: string,
  ): Promise<void> {
    if (!this.activeRunId) {
      console.warn("[ExecutionReporting] No active run to complete");
      return;
    }

    try {
      // Flush any remaining actions
      await this.flushActions();

      console.log(`[ExecutionReporting] Completing run ${this.activeRunId} with status: ${status}`);

      const finalStats = stats || this.buildExecutionStats();
      const finalCoverage = coverage || this.buildCoverageData();

      const input: ExecutionRunComplete = {
        status,
        ended_at: new Date().toISOString(),
        stats: finalStats,
        coverage: finalCoverage,
        error_message: errorMessage,
      };

      const response = await invoke<ExecutionRunCompleteResponse>("complete_execution_run", {
        runId: this.activeRunId,
        input,
      });

      console.log(
        `[ExecutionReporting] Run completed: ${response.status}, duration: ${response.duration_seconds}s`,
      );

      // Reset state
      this.activeRunId = null;
      this.activeProjectId = null;
      this.activeRunType = null;
      this.resetStats();
    } catch (error) {
      console.error("[ExecutionReporting] Failed to complete run:", error);
      // Reset state even on error
      this.activeRunId = null;
      this.activeProjectId = null;
      this.activeRunType = null;
      this.resetStats();
    }
  }

  // ============================================================================
  // Getters
  // ============================================================================

  /**
   * Check if there's an active execution run.
   */
  get isActive(): boolean {
    return this.activeRunId !== null;
  }

  /**
   * Get the current run ID.
   */
  get currentRunId(): string | null {
    return this.activeRunId;
  }

  /**
   * Get the current run type.
   */
  get currentRunType(): RunType | null {
    return this.activeRunType;
  }

  /**
   * Get the next sequence number for an action.
   * This should be called before creating ActionExecutionCreate to ensure unique sequence numbers.
   */
  getNextActionSequenceNumber(): number {
    return ++this.actionSequenceNumber;
  }

  /**
   * Get current execution stats.
   */
  get stats(): Readonly<{
    totalActions: number;
    successfulActions: number;
    failedActions: number;
    statesCovered: number;
    transitionsCovered: number;
  }> {
    return {
      totalActions: this.executionStats.totalActions,
      successfulActions: this.executionStats.successfulActions,
      failedActions: this.executionStats.failedActions,
      statesCovered: this.executionStats.statesCovered.size,
      transitionsCovered: this.executionStats.transitionsCovered.size,
    };
  }

  // ============================================================================
  // Private Methods
  // ============================================================================

  private createEmptyStats(): InternalExecutionStats {
    return {
      totalActions: 0,
      successfulActions: 0,
      failedActions: 0,
      timeoutActions: 0,
      skippedActions: 0,
      statesCovered: new Set(),
      transitionsCovered: new Set(),
      startTime: Date.now(),
      totalRecognitions: 0,
      successfulRecognitions: 0,
      failedRecognitions: 0,
    };
  }

  private resetStats(): void {
    this.executionStats = this.createEmptyStats();
    this.pendingActions = [];
    this.actionSequenceNumber = 0;
    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }
  }

  private updateStatsFromAction(action: ActionExecutionCreate): void {
    this.executionStats.totalActions++;

    switch (action.status) {
      case "success":
        this.executionStats.successfulActions++;
        break;
      case "failed":
      case "error":
        this.executionStats.failedActions++;
        break;
      case "timeout":
        this.executionStats.timeoutActions++;
        break;
      case "skipped":
        this.executionStats.skippedActions++;
        break;
    }

    // Track states covered
    if (action.from_state) {
      this.executionStats.statesCovered.add(action.from_state);
    }
    if (action.to_state) {
      this.executionStats.statesCovered.add(action.to_state);
    }
    if (action.active_states) {
      for (const state of action.active_states) {
        this.executionStats.statesCovered.add(state);
      }
    }

    // Track transitions covered
    if (action.from_state && action.to_state) {
      const transitionKey = `${action.from_state}->${action.to_state}`;
      this.executionStats.transitionsCovered.add(transitionKey);
    }
  }

  private buildExecutionStats(): ExecutionStats {
    const durationMs = Date.now() - this.executionStats.startTime;
    const totalActions = this.executionStats.totalActions;

    return {
      total_actions: totalActions,
      successful_actions: this.executionStats.successfulActions,
      failed_actions: this.executionStats.failedActions,
      timeout_actions: this.executionStats.timeoutActions,
      skipped_actions: this.executionStats.skippedActions,
      total_duration_ms: durationMs,
      avg_action_duration_ms: totalActions > 0 ? Math.round(durationMs / totalActions) : 0,
    };
  }

  private buildCoverageData(): CoverageData | undefined {
    const statesCovered = this.executionStats.statesCovered.size;
    const transitionsCovered = this.executionStats.transitionsCovered.size;

    // Only return coverage data if we have some coverage
    if (statesCovered === 0 && transitionsCovered === 0) {
      return undefined;
    }

    return {
      coverage_percentage: 0, // Would need total to calculate
      states_covered: statesCovered,
      total_states: 0, // Would need workflow metadata
      transitions_covered: transitionsCovered,
      total_transitions: 0, // Would need workflow metadata
    };
  }

  private scheduleFlush(): void {
    if (this.flushTimeout) {
      return; // Already scheduled
    }

    this.flushTimeout = setTimeout(() => {
      this.flushTimeout = null;
      this.flushActions().catch((error) => {
        console.error("[ExecutionReporting] Failed to flush actions:", error);
      });
    }, this.FLUSH_INTERVAL_MS);
  }

  private async flushActions(): Promise<void> {
    if (!this.activeRunId || this.pendingActions.length === 0) {
      return;
    }

    const actions = [...this.pendingActions];
    this.pendingActions = [];

    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }

    try {
      console.log(
        `[ExecutionReporting] Reporting ${actions.length} actions for run ${this.activeRunId}`,
      );

      const response = await invoke<ActionExecutionResponse>("report_action_executions", {
        runId: this.activeRunId,
        actions,
      });

      console.log(`[ExecutionReporting] Actions reported: ${response.recorded} recorded`);
    } catch (error) {
      console.error("[ExecutionReporting] Failed to report actions:", error);
      // Re-add actions to pending on failure (will be retried on next flush)
      this.pendingActions = [...actions, ...this.pendingActions];
    }
  }
}

// ============================================================================
// Exports
// ============================================================================

// Export singleton instance
export const executionReportingService = new ExecutionReportingServiceImpl();

// Export helper functions for direct use
export const startRun = executionReportingService.startRun.bind(executionReportingService);
export const reportActions =
  executionReportingService.reportActions.bind(executionReportingService);
export const reportAction = executionReportingService.reportAction.bind(executionReportingService);
export const reportScreenshot =
  executionReportingService.reportScreenshot.bind(executionReportingService);
export const reportIssues = executionReportingService.reportIssues.bind(executionReportingService);
export const reportImageRecognition =
  executionReportingService.reportImageRecognition.bind(executionReportingService);
export const reportTransition =
  executionReportingService.reportTransition.bind(executionReportingService);
export const completeRun = executionReportingService.completeRun.bind(executionReportingService);
export const getNextActionSequenceNumber =
  executionReportingService.getNextActionSequenceNumber.bind(executionReportingService);
