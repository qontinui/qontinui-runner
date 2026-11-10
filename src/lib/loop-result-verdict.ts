/**
 * The one reading of a unified-workflow `LoopResult`'s verdict for the
 * recap views (run recap banner, Tests tab, AI-workflow execution summary).
 *
 * A loop that ran no required verification step ends with
 * `no_verification_steps: true` and every other verdict flag false. Nothing
 * was checked, so that is neither a pass nor a refuted fix: it renders as
 * UNKNOWN ("Nothing verified"), never as "Verification Failed" and never as a
 * green "Run Completed" (served policy `verification-and-evidence`
 * `unknown-must-not-render-as-a-default`).
 *
 * The runner's `TaskRun` does not carry `loop_result` today, so these views
 * render it only once a producer fills it; this keeps the mirror honest for
 * that day rather than reading the new exit as a failure or a green default.
 */
import type { LoopResult } from "@/types/aiData";

export type LoopVerdict =
  | "stopped"
  | "critical_failure"
  | "passed"
  | "nothing_verified"
  | "max_iterations"
  | "failed";

/**
 * Classify a loop result. The first three arms follow the Rust
 * `LoopResult::summary()`: a stop wins, then a pass, then "nothing verified".
 * `unfixable_errors` and `fix_attempts_exhausted` are not mirrored on the TS
 * type, so those exits read as "failed" here (or as max iterations/critical
 * when that flag is also set).
 */
export function loopVerdict(result: LoopResult): LoopVerdict {
  if (result.was_stopped) return "stopped";
  if (result.verification_passed) return "passed";
  if (result.no_verification_steps) return "nothing_verified";
  if (result.critical_failure) return "critical_failure";
  if (result.max_iterations_reached) return "max_iterations";
  return "failed";
}

/** Short label for the "nothing verified" outcome, shared by every view. */
export const NOTHING_VERIFIED_LABEL = "Nothing Verified";
