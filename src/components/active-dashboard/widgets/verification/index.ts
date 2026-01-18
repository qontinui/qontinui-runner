/**
 * Verification Widget
 *
 * Dashboard widget for displaying verification test results.
 * Answers the question: "Did the fix work?"
 * Exports both full widget and compact summary components.
 */

export { VerificationWidget } from "./VerificationWidget";
export { VerificationSummary } from "./VerificationSummary";
export { useVerificationData } from "./useVerificationData";
export type {
  VerificationData,
  VerificationEvidence,
  VerificationStatus,
  VerificationTestType,
} from "./types";
export { DEFAULT_VERIFICATION_DATA } from "./types";
