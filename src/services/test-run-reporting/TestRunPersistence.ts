/**
 * TestRunPersistence - Module for batching and flushing data to backend
 *
 * Handles the batching logic for transitions and recognitions,
 * including scheduled flushes and retry on failure.
 *
 * @deprecated This module is part of the deprecated TestRunReportingService.
 * Use ExecutionReportingService for new implementations.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  TransitionData,
  SequencedRecognitionData,
  ReportTransitionsResponse,
  ReportRecognitionsResponse,
  ReportRecognitionsInput,
  BatchConfig,
} from "./types";
import { DEFAULT_BATCH_CONFIG } from "./types";

// ============================================================================
// Transition Batcher
// ============================================================================

/**
 * Manages batching and flushing of transition data
 */
export class TransitionBatcher {
  private pending: TransitionData[] = [];
  private flushTimeout: ReturnType<typeof setTimeout> | null = null;
  private config: BatchConfig;

  constructor(config: BatchConfig = DEFAULT_BATCH_CONFIG) {
    this.config = config;
  }

  /**
   * Add a transition to the batch
   * @returns true if batch should be flushed immediately
   */
  add(transition: TransitionData): boolean {
    this.pending.push(transition);
    return this.pending.length >= this.config.batchSize;
  }

  /**
   * Get pending transitions and clear the batch
   */
  drain(): TransitionData[] {
    const transitions = [...this.pending];
    this.pending = [];
    this.clearScheduledFlush();
    return transitions;
  }

  /**
   * Re-add transitions on failure (for retry)
   */
  prepend(transitions: TransitionData[]): void {
    this.pending = [...transitions, ...this.pending];
  }

  /**
   * Check if there are pending transitions
   */
  hasPending(): boolean {
    return this.pending.length > 0;
  }

  /**
   * Get count of pending transitions
   */
  get pendingCount(): number {
    return this.pending.length;
  }

  /**
   * Schedule a flush after the configured interval
   */
  scheduleFlush(callback: () => Promise<void>): void {
    if (this.flushTimeout) {
      return; // Already scheduled
    }

    this.flushTimeout = setTimeout(() => {
      this.flushTimeout = null;
      callback().catch((error) => {
        console.error("[TransitionBatcher] Scheduled flush failed:", error);
      });
    }, this.config.flushIntervalMs);
  }

  /**
   * Clear any scheduled flush
   */
  clearScheduledFlush(): void {
    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }
  }

  /**
   * Reset the batcher (clear pending and scheduled flush)
   */
  reset(): void {
    this.pending = [];
    this.clearScheduledFlush();
  }
}

/**
 * Flush transitions to the backend
 */
export async function flushTransitions(
  runId: string,
  transitions: TransitionData[],
): Promise<ReportTransitionsResponse> {
  console.log(`[TestRunPersistence] Reporting ${transitions.length} transitions for run ${runId}`);

  const response = await invoke<ReportTransitionsResponse>("report_test_transitions", {
    runId,
    transitions,
  });

  console.log(`[TestRunPersistence] Transitions reported: ${response.recorded} recorded`);
  return response;
}

// ============================================================================
// Recognition Batcher
// ============================================================================

/**
 * Manages batching and flushing of image recognition data
 */
export class RecognitionBatcher {
  private pending: SequencedRecognitionData[] = [];
  private flushTimeout: ReturnType<typeof setTimeout> | null = null;
  private config: BatchConfig;

  constructor(config: BatchConfig = DEFAULT_BATCH_CONFIG) {
    this.config = config;
  }

  /**
   * Add a recognition to the batch
   * @returns true if batch should be flushed immediately
   */
  add(recognition: SequencedRecognitionData): boolean {
    this.pending.push(recognition);
    return this.pending.length >= this.config.batchSize;
  }

  /**
   * Get pending recognitions and clear the batch
   */
  drain(): SequencedRecognitionData[] {
    const recognitions = [...this.pending];
    this.pending = [];
    this.clearScheduledFlush();
    return recognitions;
  }

  /**
   * Re-add recognitions on failure (for retry)
   */
  prepend(recognitions: SequencedRecognitionData[]): void {
    this.pending = [...recognitions, ...this.pending];
  }

  /**
   * Check if there are pending recognitions
   */
  hasPending(): boolean {
    return this.pending.length > 0;
  }

  /**
   * Get count of pending recognitions
   */
  get pendingCount(): number {
    return this.pending.length;
  }

  /**
   * Schedule a flush after the configured interval
   */
  scheduleFlush(callback: () => Promise<void>): void {
    if (this.flushTimeout) {
      return; // Already scheduled
    }

    this.flushTimeout = setTimeout(() => {
      this.flushTimeout = null;
      callback().catch((error) => {
        console.error("[RecognitionBatcher] Scheduled flush failed:", error);
      });
    }, this.config.flushIntervalMs);
  }

  /**
   * Clear any scheduled flush
   */
  clearScheduledFlush(): void {
    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }
  }

  /**
   * Reset the batcher (clear pending and scheduled flush)
   */
  reset(): void {
    this.pending = [];
    this.clearScheduledFlush();
  }
}

/**
 * Flush recognitions to the backend
 */
export async function flushRecognitions(
  runId: string,
  projectId: string,
  workflowId: number | null,
  recognitions: SequencedRecognitionData[],
): Promise<ReportRecognitionsResponse> {
  console.log(
    `[TestRunPersistence] Reporting ${recognitions.length} recognitions for run ${runId}`,
  );

  const input: ReportRecognitionsInput = {
    run_id: runId,
    project_id: projectId,
    workflow_id: workflowId ?? undefined,
    test_run_id: runId,
    actions: recognitions,
  };

  const response = await invoke<ReportRecognitionsResponse>("report_image_recognitions", {
    input,
  });

  console.log(`[TestRunPersistence] Recognitions reported: ${response.indexed} indexed`);
  return response;
}
