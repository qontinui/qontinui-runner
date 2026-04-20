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
import { createLogger } from "@/lib/logger";
import { ActionStatus, ActionType } from "../types/execution";
import type {
  LLMMetrics,
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
  LLMMetrics,
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

/**
 * Data for reporting a single AI prompt action (LLM call).
 */
export interface AiPromptActionData {
  name: string;
  status: "success" | "failed" | "error" | "timeout" | "skipped";
  input?: Record<string, unknown>;
  output?: Record<string, unknown>;
  error?: string;
  startTime: Date;
  endTime?: Date;
  duration_ms?: number;
  llmMetrics?: LLMMetrics;
  metadata?: Record<string, unknown>;
}

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
  // Accumulated action durations for accurate averaging
  totalActionDurationMs: number;
  // LLM tracking stats
  llmActionCount: number;
  totalTokensInput: number;
  totalTokensOutput: number;
  totalCostUsd: number;
}

// ============================================================================
// Service Implementation
// ============================================================================

const logger = createLogger("ExecutionReporting");

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
      logger.info(`Starting ${runType} run: ${runName}`);
      logger.debug(`workflowMetadata: ${JSON.stringify(workflowMetadata, null, 2)}`);

      const input: ExecutionRunCreate = {
        projectId: projectId,
        runType: runType,
        runName: runName,
        runnerMetadata: runnerMetadata,
        workflowMetadata: workflowMetadata,
        configuration,
      };

      const response = await invoke<ExecutionRunResponse>("create_execution_run", { input });

      this.activeRunId = response.runId;
      this.activeProjectId = projectId;
      this.activeRunType = runType;
      this.resetStats();

      logger.info(`Run created: ${response.runId}`);
      return response.runId;
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
      logger.info(`Uploading screenshot ${screenshot.screenshot_id} for run ${this.activeRunId}`);

      const response = await invoke<ExecutionScreenshotResponse>("upload_execution_screenshot", {
        runId: this.activeRunId,
        screenshot,
        imageData: Array.from(imageData),
      });

      logger.info(`Screenshot uploaded: ${response.image_url}`);
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
      logger.info(`Reporting ${issues.length} issues for run ${this.activeRunId}`);

      const response = await invoke<ExecutionIssueResponse>("report_execution_issues", {
        runId: this.activeRunId,
        issues,
      });

      logger.info(`Issues reported: ${response.recorded} recorded`);
    } catch (error) {
      console.error("[ExecutionReporting] Failed to report issues:", error);
    }
  }

  /**
   * Report a single AI prompt action (LLM call) to the execution service.
   *
   * @param data - The AI prompt action data
   */
  async reportAiPromptAction(data: AiPromptActionData): Promise<void> {
    if (!this.activeRunId) {
      return;
    }

    const statusMap: Record<AiPromptActionData["status"], ActionStatus> = {
      success: ActionStatus.SUCCESS,
      failed: ActionStatus.FAILED,
      error: ActionStatus.ERROR,
      timeout: ActionStatus.TIMEOUT,
      skipped: ActionStatus.SKIPPED,
    };

    const endTime = data.endTime ?? new Date();
    const durationMs = data.duration_ms ?? endTime.getTime() - data.startTime.getTime();

    const action: ActionExecutionCreate = {
      sequenceNumber: this.getNextActionSequenceNumber(),
      actionType: ActionType.AI_PROMPT,
      actionName: data.name,
      status: statusMap[data.status],
      startedAt: data.startTime.toISOString(),
      completedAt: endTime.toISOString(),
      durationMs: durationMs,
      inputData: data.input,
      outputData: data.output,
      errorMessage: data.error,
      llmMetrics: data.llmMetrics,
      spanType: "llm_call",
      metadata: data.metadata,
    };

    await this.reportAction(action);
  }

  /**
   * Report a prompt sequence: a parent "agent" span with N child "llm_call" spans.
   *
   * @param sequenceName - Name for the parent agent span
   * @param actions - Array of child AI prompt action data
   * @param parentMetadata - Optional metadata for the parent span
   */
  async reportPromptSequence(
    sequenceName: string,
    actions: AiPromptActionData[],
    parentMetadata?: Record<string, unknown>,
  ): Promise<void> {
    if (!this.activeRunId || actions.length === 0) {
      return;
    }

    const traceId = `trace-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
    const clientSpanId = `span-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

    const statusMap: Record<AiPromptActionData["status"], ActionStatus> = {
      success: ActionStatus.SUCCESS,
      failed: ActionStatus.FAILED,
      error: ActionStatus.ERROR,
      timeout: ActionStatus.TIMEOUT,
      skipped: ActionStatus.SKIPPED,
    };

    // Reserve the parent's sequence number before building children,
    // so children can reference it in metadata for parent-child linking.
    const parentSequenceNumber = this.getNextActionSequenceNumber();

    // Build child actions — parent-child linking is stored in metadata, not the
    // parent_id FK column (which expects a server-assigned UUID we don't have).
    const childActions: ActionExecutionCreate[] = actions.map((data) => {
      const endTime = data.endTime ?? new Date();
      const durationMs = data.duration_ms ?? endTime.getTime() - data.startTime.getTime();

      return {
        sequenceNumber: this.getNextActionSequenceNumber(),
        actionType: ActionType.AI_PROMPT,
        actionName: data.name,
        status: statusMap[data.status],
        startedAt: data.startTime.toISOString(),
        completedAt: endTime.toISOString(),
        durationMs: durationMs,
        inputData: data.input,
        outputData: data.output,
        errorMessage: data.error,
        llmMetrics: data.llmMetrics,
        spanType: "llm_call",
        traceId: traceId,
        metadata: {
          ...data.metadata,
          parent_span_id: clientSpanId,
          parent_sequenceNumber: parentSequenceNumber,
        },
      };
    });

    // Determine parent status from children
    const hasFailure = actions.some((a) => a.status === "failed" || a.status === "error");
    const allSkipped = actions.every((a) => a.status === "skipped");
    const parentStatus = allSkipped
      ? ActionStatus.SKIPPED
      : hasFailure
        ? ActionStatus.FAILED
        : ActionStatus.SUCCESS;

    // Calculate parent timing
    const earliestStart = actions.reduce(
      (min, a) => (a.startTime < min ? a.startTime : min),
      actions[0].startTime,
    );
    const latestEnd = actions.reduce((max, a) => {
      const end = a.endTime ?? new Date();
      return end > max ? end : max;
    }, actions[0].endTime ?? new Date());

    const parentDurationMs = latestEnd.getTime() - earliestStart.getTime();
    const aggregatedMetrics = this.aggregateLLMMetrics(actions);

    // Build parent action — includes client_span_id so children can be correlated
    // via metadata.parent_span_id == metadata.client_span_id
    const parentAction: ActionExecutionCreate = {
      sequenceNumber: parentSequenceNumber,
      actionType: ActionType.RUN_PROMPT_SEQUENCE,
      actionName: sequenceName,
      status: parentStatus,
      startedAt: earliestStart.toISOString(),
      completedAt: latestEnd.toISOString(),
      durationMs: parentDurationMs,
      llmMetrics: aggregatedMetrics,
      spanType: "agent",
      traceId: traceId,
      metadata: {
        child_count: actions.length,
        is_parent_span: true,
        client_span_id: clientSpanId,
        ...parentMetadata,
      },
    };

    // Report parent first, then children
    await this.reportAction(parentAction);
    await this.reportActions(childActions);
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
      sequenceNumber: this.getNextActionSequenceNumber(),
      actionType: ActionType.FIND,
      actionName: recognition.pattern_name || recognition.pattern_id,
      status: recognition.success ? ActionStatus.SUCCESS : ActionStatus.FAILED,
      startedAt: new Date().toISOString(),
      completedAt: new Date().toISOString(),
      durationMs: recognition.duration_ms || 0,
      patternId: recognition.pattern_id,
      patternName: recognition.pattern_name,
      activeStates: recognition.active_states,
      confidenceScore: recognition.best_match_score,
      matchLocation:
        recognition.match_x !== undefined
          ? {
              x: recognition.match_x,
              y: recognition.match_y!,
              width: recognition.match_width,
              height: recognition.match_height,
            }
          : undefined,
      outputData: {
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
      sequenceNumber: transition.sequence_number,
      actionType: ActionType.TRANSITION,
      actionName: transition.transition_name,
      status: statusMap[transition.status],
      startedAt: transition.started_at,
      completedAt: transition.completed_at,
      durationMs: transition.duration_ms,
      fromState: transition.from_state,
      toState: transition.to_state,
      errorMessage: transition.error_message,
      screenshotId: transition.screenshot_id,
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

      logger.info(`Completing run ${this.activeRunId} with status: ${status}`);

      const finalStats = stats || this.buildExecutionStats();
      const finalCoverage = coverage || this.buildCoverageData();

      const input: ExecutionRunComplete = {
        status,
        endedAt: new Date().toISOString(),
        stats: finalStats,
        coverage: finalCoverage,
        errorMessage: errorMessage,
      };

      const response = await invoke<ExecutionRunCompleteResponse>("complete_execution_run", {
        runId: this.activeRunId,
        input,
      });

      logger.info(`Run completed: ${response.status}, duration: ${response.durationSeconds}s`);

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
      totalActionDurationMs: 0,
      totalRecognitions: 0,
      successfulRecognitions: 0,
      failedRecognitions: 0,
      llmActionCount: 0,
      totalTokensInput: 0,
      totalTokensOutput: 0,
      totalCostUsd: 0,
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

    // Accumulate action duration for accurate averaging
    if (action.durationMs) {
      this.executionStats.totalActionDurationMs += action.durationMs;
    }

    // Track states covered
    if (action.fromState) {
      this.executionStats.statesCovered.add(action.fromState);
    }
    if (action.toState) {
      this.executionStats.statesCovered.add(action.toState);
    }
    if (action.activeStates) {
      for (const state of action.activeStates) {
        this.executionStats.statesCovered.add(state);
      }
    }

    // Track transitions covered
    if (action.fromState && action.toState) {
      const transitionKey = `${action.fromState}->${action.toState}`;
      this.executionStats.transitionsCovered.add(transitionKey);
    }

    // Accumulate LLM metrics
    if (action.llmMetrics) {
      this.executionStats.llmActionCount++;
      this.executionStats.totalTokensInput += action.llmMetrics.tokensInput ?? 0;
      this.executionStats.totalTokensOutput += action.llmMetrics.tokensOutput ?? 0;
      this.executionStats.totalCostUsd += action.llmMetrics.costUsd ?? 0;
    }
  }

  private buildExecutionStats(): ExecutionStats {
    const durationMs = Date.now() - this.executionStats.startTime;
    const totalActions = this.executionStats.totalActions;

    return {
      totalActions: totalActions,
      successfulActions: this.executionStats.successfulActions,
      failedActions: this.executionStats.failedActions,
      timeoutActions: this.executionStats.timeoutActions,
      skippedActions: this.executionStats.skippedActions,
      totalDurationMs: durationMs,
      avgActionDurationMs:
        totalActions > 0 ? Math.round(this.executionStats.totalActionDurationMs / totalActions) : 0,
      llmActionCount: this.executionStats.llmActionCount,
      totalTokensInput: this.executionStats.totalTokensInput,
      totalTokensOutput: this.executionStats.totalTokensOutput,
      totalCostUsd: this.executionStats.totalCostUsd,
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
      coveragePercentage: 0, // Would need total to calculate
      statesCovered: statesCovered,
      totalStates: 0, // Would need workflow metadata
      transitionsCovered: transitionsCovered,
      totalTransitions: 0, // Would need workflow metadata
    };
  }

  /**
   * Aggregate LLM metrics from multiple AI prompt actions.
   */
  private aggregateLLMMetrics(actions: AiPromptActionData[]): LLMMetrics | undefined {
    const metricsActions = actions.filter((a) => a.llmMetrics);
    if (metricsActions.length === 0) {
      return undefined;
    }

    let tokensInput = 0;
    let tokensOutput = 0;
    let costUsd = 0;

    for (const action of metricsActions) {
      const m = action.llmMetrics!;
      tokensInput += m.tokensInput ?? 0;
      tokensOutput += m.tokensOutput ?? 0;
      costUsd += m.costUsd ?? 0;
    }

    return {
      tokensInput: tokensInput,
      tokensOutput: tokensOutput,
      tokensTotal: tokensInput + tokensOutput,
      costUsd: costUsd,
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
      logger.info(`Reporting ${actions.length} actions for run ${this.activeRunId}`);

      const response = await invoke<ActionExecutionResponse>("report_action_executions", {
        runId: this.activeRunId,
        actions,
      });

      logger.info(`Actions reported: ${response.recorded} recorded`);
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
export const reportAiPromptAction =
  executionReportingService.reportAiPromptAction.bind(executionReportingService);
export const reportPromptSequence =
  executionReportingService.reportPromptSequence.bind(executionReportingService);
