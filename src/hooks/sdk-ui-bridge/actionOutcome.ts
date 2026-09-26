/**
 * actionOutcome — the ONE strict reader of a UI Bridge action result's
 * `success` field on the TypeScript side.
 *
 * Three states, not two, because "absent" is not "true". A body with no boolean
 * `success` key — a malformed reply, a non-envelope error page, a proxy's own
 * JSON, a `null` result — used to read as success through `json.success !==
 * false` / `!r || r.success !== false` defaults. Same polarity as
 * `recoveryScope.ts` (`r.success === true`), the SDK's own
 * `ChangeTracker.executeWithDiff` read, and the Rust twin
 * `mcp::ui_bridge::request::action_outcome`.
 *
 * Plan: 2026-09-10-two-runner-call-sites-still-report-success-for-an-action-that-did-not-happen
 */

/** Error text for a result that carried no boolean `success`. The prefix is
 * the distinguishable half: "I could not tell" is not "the action failed". */
export const INDETERMINATE_ACTION_ERROR =
  "INDETERMINATE: action result carried no boolean `success` field";

export type ActionOutcome =
  | { kind: "succeeded" }
  | { kind: "failed"; error: string | null }
  | { kind: "indeterminate" };

/** Strictly read `result.success`: `true` → succeeded, `false` → failed,
 * anything else (absent, non-boolean, non-object result) → indeterminate. */
export function actionOutcome(result: unknown): ActionOutcome {
  if (result === null || typeof result !== "object") return { kind: "indeterminate" };
  const r = result as { success?: unknown; error?: unknown };
  if (r.success === true) return { kind: "succeeded" };
  if (r.success === false) {
    return { kind: "failed", error: typeof r.error === "string" ? r.error : null };
  }
  return { kind: "indeterminate" };
}

/** `true` only for `succeeded` — indeterminate is never a success. */
export function actionSucceeded(outcome: ActionOutcome): boolean {
  return outcome.kind === "succeeded";
}

/** The error to report for a non-success outcome (`undefined` on success). */
export function actionOutcomeError(outcome: ActionOutcome, fallback: string): string | undefined {
  switch (outcome.kind) {
    case "succeeded":
      return undefined;
    case "failed":
      return outcome.error ?? fallback;
    case "indeterminate":
      return INDETERMINATE_ACTION_ERROR;
  }
}

/** The verdict fields of a `CommandResult` built from one HTTP relay reply. */
export interface RelayVerdict {
  success: boolean;
  error?: string;
  outcome: ActionOutcome["kind"];
}

/**
 * Verdict for a runner HTTP relay reply (`useCommands` `executeAction` /
 * `sendCommand`). Success needs BOTH a 2xx and an explicit `success: true`;
 * a body without the key is indeterminate — reported as a failure with the
 * `INDETERMINATE:` error, or with the HTTP status when the reply was non-2xx.
 */
export function relayVerdict(body: unknown, resp: { ok: boolean; status: number }): RelayVerdict {
  const outcome = actionOutcome(body);
  const bodyError =
    body !== null &&
    typeof body === "object" &&
    typeof (body as { error?: unknown }).error === "string"
      ? (body as { error: string }).error
      : undefined;
  if (outcome.kind === "succeeded") {
    if (resp.ok) return { success: true, error: bodyError, outcome: outcome.kind };
    return { success: false, error: bodyError ?? `HTTP ${resp.status}`, outcome: "failed" };
  }
  if (outcome.kind === "indeterminate" && !resp.ok) {
    return { success: false, error: bodyError ?? `HTTP ${resp.status}`, outcome: "failed" };
  }
  return {
    success: false,
    error: actionOutcomeError(outcome, "action reported success: false"),
    outcome: outcome.kind,
  };
}

/**
 * Verdict for a runner HTTP relay reply to a plain DATA READ (snapshot,
 * elements, health…). Those routes may forward raw app JSON with no `success`
 * key, so a read succeeds on a 2xx whose body is not an explicit
 * `success: false`. Use {@link relayVerdict} for action results instead.
 */
export function readVerdict(body: unknown, resp: { ok: boolean; status: number }): RelayVerdict {
  const bodyError =
    body !== null &&
    typeof body === "object" &&
    typeof (body as { error?: unknown }).error === "string"
      ? (body as { error: string }).error
      : undefined;
  const explicitFailure =
    body !== null && typeof body === "object" && (body as { success?: unknown }).success === false;
  if (resp.ok && !explicitFailure) return { success: true, error: bodyError, outcome: "succeeded" };
  return { success: false, error: bodyError ?? `HTTP ${resp.status}`, outcome: "failed" };
}
