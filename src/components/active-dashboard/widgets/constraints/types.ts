/**
 * Constraint Results Widget Types
 *
 * Type definitions for the constraint evaluation results widget.
 * Maps to the Rust ConstraintResults AppEvent and ConstraintResult types.
 */

import type { ConstraintResult } from "@qontinui/shared-types/constraints";

/**
 * Per-iteration constraint evaluation results received from the Rust backend.
 */
export interface ConstraintIterationResults {
  /** Task run ID */
  taskRunId: string;
  /** Iteration number (1-indexed) */
  iteration: number;
  /** Human-readable summary (e.g., "all 3 constraints passed") */
  summary: string;
  /** Whether any blocking violations exist */
  hasBlocking: boolean;
  /** Individual constraint results */
  results: ConstraintResult[];
  /** Timestamp when received */
  receivedAt: number;
}

/**
 * Aggregate data for the constraint results widget.
 */
export interface ConstraintResultsData {
  /** All iterations received, newest first */
  iterations: ConstraintIterationResults[];
  /** Whether any data has been received */
  hasData: boolean;
  /** The latest iteration's results */
  latest: ConstraintIterationResults | null;
  /** Counts across all constraints in the latest iteration */
  latestCounts: ConstraintCounts;
}

/**
 * Summary counts for a set of constraint results.
 */
export interface ConstraintCounts {
  total: number;
  passed: number;
  failed: number;
  blocking: number;
  warnings: number;
  logs: number;
}

/**
 * Default empty state.
 */
export const DEFAULT_CONSTRAINT_RESULTS_DATA: ConstraintResultsData = {
  iterations: [],
  hasData: false,
  latest: null,
  latestCounts: {
    total: 0,
    passed: 0,
    failed: 0,
    blocking: 0,
    warnings: 0,
    logs: 0,
  },
};

/**
 * Calculate counts from a list of constraint results.
 */
export function calculateCounts(results: ConstraintResult[]): ConstraintCounts {
  const counts: ConstraintCounts = {
    total: results.length,
    passed: 0,
    failed: 0,
    blocking: 0,
    warnings: 0,
    logs: 0,
  };

  for (const r of results) {
    if (r.passed) {
      counts.passed++;
    } else {
      counts.failed++;
      if (r.severity === "block") counts.blocking++;
      else if (r.severity === "warn") counts.warnings++;
      else if (r.severity === "log") counts.logs++;
    }
  }

  return counts;
}

// Re-export shared types used by the widget
export type {
  ConstraintResult,
  ConstraintSeverity,
  ConstraintViolation,
} from "@qontinui/shared-types/constraints";
