/**
 * Verification Widget Types
 *
 * Type definitions for the verification result widget.
 * This widget answers "Did the fix work?" by showing verification test results.
 */

/**
 * Status of the verification process.
 */
export type VerificationStatus = "pending" | "running" | "passed" | "failed" | "skipped";

/**
 * Type of verification test that was run.
 */
export type VerificationTestType = "repo_test" | "playwright" | "gui_automation";

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
};
