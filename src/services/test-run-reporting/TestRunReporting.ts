/**
 * TestRunReporting - Coordinator module for test run reporting
 *
 * This module provides the main service class that coordinates:
 * - Test run creation
 * - Transition and recognition reporting (with batching)
 * - Stats tracking
 * - Test run completion
 *
 * @deprecated This module is part of the deprecated TestRunReportingService.
 * Use ExecutionReportingService for new implementations.
 */

import type {
  TransitionData,
  ImageRecognitionData,
  SequencedRecognitionData,
  ExecutionStats,
  ActiveRunState,
  PublicExecutionStats,
  BatchConfig,
} from "./types";
import { DEFAULT_BATCH_CONFIG } from "./types";
import {
  createTestRun,
  createActiveRunState,
  createEmptyRunState,
  type StartTestRunOptions,
} from "./TestRunCreation";
import {
  createExecutionStats,
  updateStatsForTransition,
  updateStatsForRecognition,
  toPublicStats,
} from "./TestRunStats";
import {
  TransitionBatcher,
  RecognitionBatcher,
  flushTransitions,
  flushRecognitions,
} from "./TestRunPersistence";
import { completeTestRun } from "./TestRunCompletion";

/**
 * Main service class for test run reporting
 *
 * @deprecated Use ExecutionReportingService instead.
 * This class is kept for backward compatibility with existing /api/v1/testing/* endpoints.
 */
export class TestRunReportingServiceImpl {
  private activeRunState: ActiveRunState = createEmptyRunState();
  private executionStats: ExecutionStats = createExecutionStats();
  private transitionBatcher: TransitionBatcher;
  private recognitionBatcher: RecognitionBatcher;
  private transitionSequenceNumber = 0;
  private recognitionSequenceNumber = 0;

  constructor(config: BatchConfig = DEFAULT_BATCH_CONFIG) {
    this.transitionBatcher = new TransitionBatcher(config);
    this.recognitionBatcher = new RecognitionBatcher(config);
  }

  // ============================================================================
  // Public API - Run Lifecycle
  // ============================================================================

  /**
   * Start a new test run for the given workflow
   */
  async startTestRun(
    projectId: string,
    workflowId: string,
    workflowName: string,
    options?: StartTestRunOptions,
  ): Promise<string | null> {
    const response = await createTestRun(projectId, workflowId, workflowName, options);

    if (response) {
      this.activeRunState = createActiveRunState(response, projectId, workflowId);
      this.resetInternal();
      return response.run_id;
    }

    return null;
  }

  /**
   * Complete the active test run with final status
   */
  async completeTestRun(success: boolean): Promise<void> {
    if (!this.activeRunState.runId) {
      console.warn("[TestRunReporting] No active test run to complete");
      return;
    }

    try {
      // Flush any remaining data
      await this.flushAllPending();

      await completeTestRun(this.activeRunState.runId, success, this.executionStats);
    } catch (error) {
      console.error("[TestRunReporting] Failed to complete test run:", error);
    } finally {
      // Always reset state
      this.activeRunState = createEmptyRunState();
      this.resetInternal();
    }
  }

  // ============================================================================
  // Public API - Reporting
  // ============================================================================

  /**
   * Report a transition that occurred during execution
   */
  async reportTransition(transition: TransitionData): Promise<void> {
    if (!this.activeRunState.runId) {
      return; // No active test run
    }

    // Update stats
    updateStatsForTransition(this.executionStats, transition);

    // Add to batch
    const shouldFlush = this.transitionBatcher.add(transition);

    if (shouldFlush) {
      await this.flushTransitionsInternal();
    } else {
      this.transitionBatcher.scheduleFlush(() => this.flushTransitionsInternal());
    }
  }

  /**
   * Report an image recognition result for historical storage
   */
  async reportImageRecognition(data: ImageRecognitionData): Promise<void> {
    if (!this.activeRunState.runId || !this.activeRunState.projectId) {
      return; // No active test run
    }

    // Update stats
    updateStatsForRecognition(this.executionStats, data);

    // Create sequenced data
    this.recognitionSequenceNumber++;
    const sequencedData: SequencedRecognitionData = {
      ...data,
      action_id: `recognition-${Date.now()}-${this.recognitionSequenceNumber}`,
      sequence_number: this.recognitionSequenceNumber,
    };

    // Add to batch
    const shouldFlush = this.recognitionBatcher.add(sequencedData);

    if (shouldFlush) {
      await this.flushRecognitionsInternal();
    } else {
      this.recognitionBatcher.scheduleFlush(() => this.flushRecognitionsInternal());
    }
  }

  // ============================================================================
  // Public API - Accessors
  // ============================================================================

  /**
   * Check if there's an active test run
   */
  get isActive(): boolean {
    return this.activeRunState.runId !== null;
  }

  /**
   * Get the current run ID
   */
  get currentRunId(): string | null {
    return this.activeRunState.runId;
  }

  /**
   * Get current execution stats (read-only)
   */
  get stats(): PublicExecutionStats {
    return toPublicStats(this.executionStats);
  }

  /**
   * Get the next sequence number for a transition
   */
  getNextTransitionSequenceNumber(): number {
    return ++this.transitionSequenceNumber;
  }

  // ============================================================================
  // Private Methods
  // ============================================================================

  private resetInternal(): void {
    this.executionStats = createExecutionStats();
    this.transitionBatcher.reset();
    this.recognitionBatcher.reset();
    this.transitionSequenceNumber = 0;
    this.recognitionSequenceNumber = 0;
  }

  private async flushAllPending(): Promise<void> {
    await Promise.all([this.flushTransitionsInternal(), this.flushRecognitionsInternal()]);
  }

  private async flushTransitionsInternal(): Promise<void> {
    if (!this.activeRunState.runId || !this.transitionBatcher.hasPending()) {
      return;
    }

    const transitions = this.transitionBatcher.drain();

    try {
      await flushTransitions(this.activeRunState.runId, transitions);
    } catch (error) {
      console.error("[TestRunReporting] Failed to flush transitions:", error);
      // Re-add for retry
      this.transitionBatcher.prepend(transitions);
    }
  }

  private async flushRecognitionsInternal(): Promise<void> {
    const { runId, projectId, workflowId } = this.activeRunState;

    if (!runId || !projectId || !this.recognitionBatcher.hasPending()) {
      return;
    }

    const recognitions = this.recognitionBatcher.drain();

    try {
      await flushRecognitions(runId, projectId, workflowId, recognitions);
    } catch (error) {
      console.error("[TestRunReporting] Failed to flush recognitions:", error);
      // Re-add for retry
      this.recognitionBatcher.prepend(recognitions);
    }
  }
}
