/**
 * Verification Widget Types
 *
 * Type definitions for the verification result widget.
 * This widget answers "Did the fix work?" by showing verification test results.
 */

import type { StepStats, StepExecutionStatus } from "../shared/types";

/**
 * Status of the verification process.
 */
export type VerificationStatus = "pending" | "running" | "passed" | "failed" | "skipped";

/**
 * Type of verification test that was run.
 */
export type VerificationTestType = "repo_test" | "playwright" | "gui_automation" | "check_group";

/**
 * Evidence collected during verification.
 */
export interface VerificationEvidence {
  /** Type of evidence */
  type: "screenshot" | "log" | "dom_snapshot" | "console";
  /** File path for screenshot or dom_snapshot evidence */
  path?: string;
  /** Inline content for log or console evidence */
  content?: string;
  /** When this evidence was captured */
  timestamp: number;
}

/**
 * Individual check step in verification.
 */
export interface CheckStep {
  /** Unique step ID */
  id: string;
  /** Step name/description */
  name: string;
  /** Execution status */
  status: StepExecutionStatus;
  /** Check type (check_group, check, playwright, etc.) */
  checkType: VerificationTestType;
  /** Start timestamp */
  startTime?: number;
  /** End timestamp */
  endTime?: number;
  /** Duration in ms */
  durationMs?: number;
  /** Error message if failed */
  error?: string;
  /** Output/result preview */
  output?: string;
}

/**
 * Complete verification data for the widget.
 */
export interface VerificationData {
  /** Current verification status */
  status: VerificationStatus;
  /** Type of test used for verification (null if pending/skipped) */
  testType: VerificationTestType | null;
  /** Name of the test that was run (null if pending/skipped) */
  testName: string | null;
  /** Human-readable description of what was verified */
  description: string | null;
  /** Evidence collected during verification */
  evidence: VerificationEvidence[];
  /** Duration of verification in milliseconds */
  durationMs: number;
  /** Error message if verification failed */
  error?: string;
  /** List of check steps (for step-based verification) */
  checkSteps: CheckStep[];
  /** Overall statistics for check steps */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
}

/**
 * Default empty state for verification data.
 */
export const DEFAULT_VERIFICATION_DATA: VerificationData = {
  status: "pending",
  testType: null,
  testName: null,
  description: null,
  evidence: [],
  durationMs: 0,
  checkSteps: [],
  stats: {
    total: 0,
    completed: 0,
    successful: 0,
    failed: 0,
    pending: 0,
    elapsedTime: 0,
    successRate: 100,
  },
  isLoading: true,
};
