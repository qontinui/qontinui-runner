/**
 * TestRunStats - Module for tracking execution statistics
 *
 * @deprecated This module is part of the deprecated TestRunReportingService.
 * Use ExecutionReportingService for new implementations.
 */

import type {
  ExecutionStats,
  PublicExecutionStats,
  TransitionData,
  ImageRecognitionData,
} from "./types";

/**
 * Creates a fresh execution stats object
 */
export function createExecutionStats(): ExecutionStats {
  return {
    totalTransitions: 0,
    successfulTransitions: 0,
    failedTransitions: 0,
    statesCovered: new Set(),
    totalRecognitions: 0,
    successfulRecognitions: 0,
    failedRecognitions: 0,
  };
}

/**
 * Update stats based on a transition
 */
export function updateStatsForTransition(stats: ExecutionStats, transition: TransitionData): void {
  stats.totalTransitions++;

  if (transition.status === "success") {
    stats.successfulTransitions++;
  } else if (transition.status === "failed") {
    stats.failedTransitions++;
  }

  // Track states covered
  if (transition.from_state) {
    stats.statesCovered.add(transition.from_state);
  }
  if (transition.to_state) {
    stats.statesCovered.add(transition.to_state);
  }
}

/**
 * Update stats based on an image recognition result
 */
export function updateStatsForRecognition(stats: ExecutionStats, data: ImageRecognitionData): void {
  stats.totalRecognitions++;

  if (data.success) {
    stats.successfulRecognitions++;
  } else {
    stats.failedRecognitions++;
  }

  // Track states covered from active_states
  for (const state of data.active_states) {
    stats.statesCovered.add(state);
  }
}

/**
 * Convert internal stats to public read-only format
 */
export function toPublicStats(stats: ExecutionStats): PublicExecutionStats {
  return {
    totalTransitions: stats.totalTransitions,
    successfulTransitions: stats.successfulTransitions,
    failedTransitions: stats.failedTransitions,
    statesCovered: stats.statesCovered.size,
  };
}

/**
 * Get the count of states covered
 */
export function getStatesCoveredCount(stats: ExecutionStats): number {
  return stats.statesCovered.size;
}
