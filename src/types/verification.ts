/**
 * Types for AI self-healing verification workflow
 */

/**
 * Pending verification state that survives runner restarts
 */
export interface PendingVerification {
  /** Unique identifier for this verification */
  id: string;
  /** Timestamp when the verification was created (ms since epoch) */
  created_at: number;
  /** Reason for the verification */
  reason: "fix_applied" | "restart_requested";
  /** Summary of what was being discussed */
  conversation_summary: string;
  /** Issue IDs that were fixed */
  issues_fixed: string[];
  /** Files that were modified */
  files_modified: string[];
  /** Prompt to run for verification */
  verification_prompt: string;
  /** Current status */
  status: "pending" | "in_progress" | "completed" | "failed";
}

/**
 * Verification pending marker parsed from AI output
 * AI outputs: [VERIFICATION:PENDING] {"reason":"fix_applied",...}
 */
export interface VerificationPendingMarker {
  reason: "fix_applied" | "restart_requested";
  files_modified: string[];
  verification_prompt: string;
  conversation_summary?: string;
  issues_fixed?: string[];
}

/**
 * Verification completed marker parsed from AI output
 * AI outputs: [VERIFICATION:COMPLETED] {"success":true,...}
 */
export interface VerificationCompletedMarker {
  success: boolean;
  message: string;
}

/**
 * Verification failed marker parsed from AI output
 * AI outputs: [VERIFICATION:FAILED] {"success":false,...}
 */
export interface VerificationFailedMarker {
  success: false;
  message: string;
  next_steps?: string;
}

/**
 * Runner restart marker parsed from AI output
 * AI outputs: [RUNNER:RESTART] {"reason":"Applied fix",...}
 */
export interface RunnerRestartMarker {
  reason: string;
  delay_seconds?: number;
}

/**
 * Union type for all verification-related markers
 */
export type VerificationMarker =
  | { type: "pending"; data: VerificationPendingMarker }
  | { type: "completed"; data: VerificationCompletedMarker }
  | { type: "failed"; data: VerificationFailedMarker }
  | { type: "restart"; data: RunnerRestartMarker };
