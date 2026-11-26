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

/** Error text for an envelope whose nested action result (`data.success`)
 * is present but not a boolean. Same `INDETERMINATE:` prefix. */
export const INDETERMINATE_NESTED_ACTION_ERROR =
  "INDETERMINATE: nested action result `data.success` is not a boolean";

/**
 * Strictly read an `APIResponse` ENVELOPE (`{ success, data, error }`) whose
 * `data` may itself be an action result carrying its own `success`.
 *
 * The SDK's in-process/HTTP `aiExecute` handler answers
 * `success(nlActionResponse)` — outer `success: true` even when the nested
 * `NLActionResponse.success` is `false` — so the outer flag alone reports a
 * refused action as done. The verdict is therefore strict on BOTH levels:
 *
 * - outer not `true` → the outer {@link actionOutcome} (failed / indeterminate);
 * - outer `true`, `data` an object with NO `success` key (the relay's bare,
 *   already-lifted shape, or any non-action payload) → governed by the outer;
 * - outer `true`, `data.success === true` → succeeded;
 * - outer `true`, `data.success === false` → failed with the INNER error
 *   (`data.error`, else `data.failureInfo.message` — `NLActionResponse`'s
 *   structured field);
 * - outer `true`, `data.success` present but non-boolean → indeterminate.
 *
 * Plan: 2026-09-27-ui-bridge-action-failures-still-masked-after-the-strict-success-readers (Case 3)
 */
export function envelopeOutcome(body: unknown): ActionOutcome {
  const outer = actionOutcome(body);
  if (outer.kind !== "succeeded") return outer;
  const data = (body as { data?: unknown }).data;
  if (data === null || typeof data !== "object" || Array.isArray(data)) return outer;
  if (!Object.prototype.hasOwnProperty.call(data, "success")) return outer;
  const inner = data as { success?: unknown; error?: unknown; failureInfo?: unknown };
  if (inner.success === true) return { kind: "succeeded" };
  if (inner.success === false) return { kind: "failed", error: nestedError(inner) };
  return { kind: "indeterminate" };
}

/** The inner action result's error: `error`, else `failureInfo.message`. */
function nestedError(inner: { error?: unknown; failureInfo?: unknown }): string | null {
  if (typeof inner.error === "string" && inner.error.length > 0) return inner.error;
  const info = inner.failureInfo;
  if (info !== null && typeof info === "object") {
    const message = (info as { message?: unknown }).message;
    if (typeof message === "string" && message.length > 0) return message;
  }
  return null;
}

/**
 * Verdict for a runner HTTP relay reply (`useCommands` `executeAction` /
 * `sendCommand`). Success needs a 2xx AND an explicit outer `success: true`
 * AND — when `data` nests an action result — an inner `success: true`
 * ({@link envelopeOutcome}). A body without the key is indeterminate —
 * reported as a failure with the `INDETERMINATE:` error, or with the HTTP
 * status when the reply was non-2xx.
 */
export function relayVerdict(body: unknown, resp: { ok: boolean; status: number }): RelayVerdict {
  const outer = actionOutcome(body);
  const bodyError =
    body !== null &&
    typeof body === "object" &&
    typeof (body as { error?: unknown }).error === "string"
      ? (body as { error: string }).error
      : undefined;
  if (!resp.ok && outer.kind !== "failed") {
    return { success: false, error: bodyError ?? `HTTP ${resp.status}`, outcome: "failed" };
  }
  if (outer.kind !== "succeeded") {
    return {
      success: false,
      error: actionOutcomeError(outer, "action reported success: false"),
      outcome: outer.kind,
    };
  }
  // 2xx + outer success: the nested action result, when present, decides.
  const outcome = envelopeOutcome(body);
  switch (outcome.kind) {
    case "succeeded":
      return { success: true, error: bodyError, outcome: outcome.kind };
    case "failed":
      return {
        success: false,
        error: outcome.error ?? bodyError ?? "nested action reported success: false",
        outcome: outcome.kind,
      };
    case "indeterminate":
      return { success: false, error: INDETERMINATE_NESTED_ACTION_ERROR, outcome: outcome.kind };
  }
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
