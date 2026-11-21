/**
 * Scoping and write-refusal rules for the `ai_recovery_attempt` handler.
 *
 * THE DEFECT this exists to close: the handler discovered EVERY element on the
 * page (`includeHidden: true`), handed the whole set to `NLActionExecutor`
 * along with a free-text instruction built from the failure message, and let
 * it pick whatever it matched best. A failed action therefore wrote into an
 * element the caller had never addressed — a failing write elsewhere on the
 * page left its text sitting in the command palette's input. The handler then
 * reported `recovered: true` unconditionally, so the response asserted a
 * successful recovery of an action that was still broken.
 *
 * Three rules, enforced here rather than trusted to the executor's matching:
 *
 * 1. **Scoped** — recovery may only touch the element the caller addressed.
 * 2. **Never writes** — recovery repositions (scroll, focus, wait, resnapshot);
 *    it does not invent input on the caller's behalf.
 * 3. **Honest** — `attemptSucceeded` is read off the executor's own verdict.
 *
 * A leaf module (its only import is the generated write-action JSON), for the
 * same reason `terminalWriteResult.ts` is one: `useAISearchEvents` pulls React
 * and the bridge, so nothing exported from it is testable under the runner's
 * `environment: "node"` vitest config.
 */

import writeActionsGenerated from "./writeActions.generated.json";

/** Machine-readable refusal: the caller did not say which element to recover. */
export const RECOVERY_UNSCOPED = "RECOVERY_UNSCOPED";
/** Machine-readable refusal: the recovery step addressed a different element. */
export const RECOVERY_OUT_OF_SCOPE = "RECOVERY_OUT_OF_SCOPE";
/** Machine-readable refusal: the recovery step tried to mutate input state. */
export const RECOVERY_WRITE_REFUSED = "RECOVERY_WRITE_REFUSED";
/** Machine-readable refusal: the addressed element is not in the current tree. */
export const RECOVERY_TARGET_MISSING = "RECOVERY_TARGET_MISSING";
/** Machine-readable outcome: recovery RAN against the addressed element and
 *  did not recover it. Not a refusal — an honest negative result — but still a
 *  failure, so it must never reach the caller as an HTTP 200 success. */
export const RECOVERY_FAILED = "RECOVERY_FAILED";
/** Machine-readable refusal: the executor answered without dispatching any
 *  action to the bridge (the instruction did not parse, matched nothing on the
 *  addressed element, or fell below the confidence threshold). Nothing ran, so
 *  it is a refusal before dispatch — never `RECOVERY_FAILED`, which says the
 *  original action ran and failed. */
export const RECOVERY_NOT_DISPATCHED = "RECOVERY_NOT_DISPATCHED";

/**
 * Actions that MUTATE input state. The LIST is not declared here: it is the
 * runner's `WRITE_ACTIONS` constant in
 * `src-tauri/src/mcp/ui_bridge/recovery_executor.rs`, generated into
 * `writeActions.generated.json` and drift-gated by the Rust test
 * `write_actions_fixture_matches_rust_declaration`. The CHECK stays two —
 * this `isWriteAction` and the runner's `is_write_action` both refuse, so
 * neither is the single point of failure across the IPC seam — but a string
 * can no longer be added to one side's list alone.
 */
const WRITE_ACTIONS: ReadonlySet<string> = new Set<string>(writeActionsGenerated);

/** True when `action` would mutate input state, so recovery must refuse it. */
export function isWriteAction(action: string): boolean {
  return WRITE_ACTIONS.has(action.trim().toLowerCase());
}

/** An action a recovery step was refused. Carries a machine-readable `code`. */
export class RecoveryRefusedError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "RecoveryRefusedError";
    this.code = code;
  }
}

/**
 * Gate one recovery dispatch. Throws {@link RecoveryRefusedError} when the
 * dispatch leaves the addressed element or would write.
 */
export function assertRecoveryDispatchAllowed(
  targetElementId: string,
  elementId: string,
  action: string,
): void {
  if (!targetElementId) {
    throw new RecoveryRefusedError(
      RECOVERY_UNSCOPED,
      `${RECOVERY_UNSCOPED}: recovery ran without an addressed element; refusing to guess a target.`,
    );
  }
  if (elementId !== targetElementId) {
    throw new RecoveryRefusedError(
      RECOVERY_OUT_OF_SCOPE,
      `${RECOVERY_OUT_OF_SCOPE}: recovery for '${targetElementId}' tried to act on '${elementId}'.`,
    );
  }
  if (isWriteAction(action)) {
    throw new RecoveryRefusedError(
      RECOVERY_WRITE_REFUSED,
      `${RECOVERY_WRITE_REFUSED}: recovery may not '${action}' — it never writes on the caller's behalf.`,
    );
  }
}

/**
 * What the scoped executor actually did, filled in by
 * {@link scopeRecoveryExecutor}. `NLActionExecutor.execute` catches every
 * failure — a parse miss, no matching element, low confidence, and this
 * module's own {@link RecoveryRefusedError} — and RETURNS a failure response,
 * so its answer alone cannot say whether anything reached the bridge. This
 * can: `dispatched` turns true only once a call has passed the guard and been
 * handed to the bridge, and `refusal` keeps the guard's own refusal.
 */
export interface RecoveryDispatchRecord {
  dispatched: boolean;
  refusal: RecoveryRefusedError | null;
}

export function newRecoveryDispatchRecord(): RecoveryDispatchRecord {
  return { dispatched: false, refusal: null };
}

/**
 * Wrap a UI Bridge instance so every action it dispatches during recovery is
 * checked by {@link assertRecoveryDispatchAllowed} first. Component actions
 * and form fills are refused outright — neither is element-scoped, so neither
 * can be constrained to the addressed element. When `record` is given, it is
 * told whether any call got past the guard (see {@link RecoveryDispatchRecord}).
 */
export function scopeRecoveryExecutor<T extends object>(
  bridge: T,
  targetElementId: string,
  record?: RecoveryDispatchRecord,
): T {
  return new Proxy(bridge, {
    get(target, prop, receiver) {
      const value = Reflect.get(target, prop, receiver);
      if (prop === "executeAction" && typeof value === "function") {
        // `async` so a refusal surfaces as a REJECTED promise: the executor
        // awaits this call, and a synchronous throw from an async-shaped API
        // is a footgun for anything that builds the promise before awaiting.
        return async (
          elementId: string,
          action: { action?: string } | undefined,
          ...rest: unknown[]
        ) => {
          try {
            assertRecoveryDispatchAllowed(targetElementId, elementId, action?.action ?? "");
          } catch (err) {
            if (record && err instanceof RecoveryRefusedError) record.refusal = err;
            throw err;
          }
          // Set BEFORE the call: a call that throws mid-action may have partly
          // applied, so from here on the action counts as dispatched.
          if (record) record.dispatched = true;
          return (value as (...a: unknown[]) => unknown).call(target, elementId, action, ...rest);
        };
      }
      if (
        (prop === "executeComponentAction" || prop === "fillForm") &&
        typeof value === "function"
      ) {
        return async () => {
          const refusal = new RecoveryRefusedError(
            RECOVERY_OUT_OF_SCOPE,
            `${RECOVERY_OUT_OF_SCOPE}: '${String(prop)}' is not element-scoped; recovery is limited to '${targetElementId}'.`,
          );
          if (record) record.refusal = refusal;
          throw refusal;
        };
      }
      return typeof value === "function"
        ? (value as (...a: unknown[]) => unknown).bind(target)
        : value;
    },
  }) as T;
}

/**
 * The honest verdict on the SCOPED ATTEMPT, read off the executor's own
 * response.
 *
 * Deliberately NOT named `recovered`. The runner's
 * `RecoveryOutcome.recovered` (`recovery_executor.rs`) answers a different,
 * stricter question — "does the ORIGINAL action now succeed?", re-verified by
 * an independent retry. This answers only "did the scoped executor act on the
 * addressed element?". The two can legitimately disagree (the right element
 * was clicked, the original action is still broken: attempt `true`, outer
 * `false`), and one word for both hid which question was being answered.
 */
export interface RecoveryVerdict {
  /** `true` ONLY when the scoped executor itself reported success on the
   *  addressed element. Says nothing about the original action. */
  attemptSucceeded: boolean;
  /** The executor's failure reason, when it failed. */
  reason: string | null;
}

/**
 * Derive `attemptSucceeded` from the executor's response instead of asserting it.
 *
 * The old handler hardcoded `recovered: true` on any resolved executor call,
 * so a recovery that found no element, fell below the confidence threshold, or
 * threw mid-action still reported success to the caller.
 */
export function recoveryVerdict(result: unknown): RecoveryVerdict {
  if (result === null || typeof result !== "object") {
    return { attemptSucceeded: false, reason: "recovery executor returned no result" };
  }
  const r = result as { success?: unknown; error?: unknown; errorCode?: unknown };
  if (r.success === true) return { attemptSucceeded: true, reason: null };
  const code = typeof r.errorCode === "string" ? r.errorCode : null;
  const message = typeof r.error === "string" ? r.error : "recovery did not succeed";
  return { attemptSucceeded: false, reason: code ? `${code}: ${message}` : message };
}

/**
 * Build the `data` payload for a FAILED `ai_recovery_attempt` response.
 *
 * ## What this used to have to do, and no longer does
 *
 * The runner's response dispatcher used to forward ONLY `response.data` when a
 * handler supplied one, dropping the sibling `success: false` and `error` on
 * the floor — so `wrap_ipc_result` saw a bare `{recovered: false}`, found no
 * `success: false` to flatten, and answered **HTTP 200
 * `{"success":true,"recovered":false}`**. A caller that refused to guess a
 * target read as a caller that succeeded. Every handler that wanted an honest
 * answer had to mirror `success`/`error` into `data` itself, which is what this
 * helper was for.
 *
 * That seam is fixed at the seam: `extract_response_data`
 * (`src-tauri/src/mcp/ui_bridge/request.rs`) now stamps an envelope-level
 * `success: false` — plus `error`, `code` and `hint` — onto the forwarded
 * payload for EVERY handler, so the per-call-site mirroring is gone from here.
 *
 * What remains is the part that was never envelope duplication:
 * `attemptSucceeded`, the scoped-attempt verdict `as_recovery_failure`
 * (`ai_analyze.rs`) keys its Ok-arm on, and
 * `code`, which stays alongside it so a data-only reader still sees the typed
 * token. Fields already present here win over the stamped ones, and `extra` is
 * spread FIRST so it can never overwrite `code` or the verdict — setting
 * `attemptSucceeded: true` from a failure path is not possible by accident.
 */
export function recoveryFailureData(
  code: string,
  extra: Record<string, unknown> = {},
): Record<string, unknown> {
  return { ...extra, code, attemptSucceeded: false };
}

/**
 * Whether the ORIGINAL action ran inside one `ai_recovery_attempt`, stated
 * the way the ui-bridge TS SDK server's `attemptRecovery` states it
 * (`originalActionAttempted` / `originalActionPerformed`), so a client reading
 * either server decides the same way (plan
 * `2026-11-18-the-journey-ledger-never-records-an-unknown-action-verdict-as-a-success-or-a-failure`,
 * Phase 3).
 *
 * The route's `instruction` IS the original instruction (the same contract as
 * the TS server's `request.instruction`), and this executor runs it directly on
 * the addressed element. So:
 *
 * - `started: false` — the executor was never reached (an unscoped request, a
 *   missing target, a throw before `execute`): nothing ran, so both are
 *   `false`. Same default as the TS server's refusal arm.
 * - `started: true` with an instruction — the original ran (once).
 *   `originalActionAttempted` is `true` even when it reported failure or threw,
 *   because a failed action may have partly applied (the TS server sets its
 *   `ranOriginal` before the call for the same reason), and
 *   `originalActionPerformed` is `true` only when the executor reported success.
 * - `started: true` with NO instruction — the executor ran a generic
 *   "recover from error state" step, and the request named no original to
 *   compare it with. Both fields are OMITTED: an absent field is UNKNOWN to
 *   the ui-bridge Python client (`read_recovery_attempt`), which then never
 *   re-executes. Stating `false` would license a retry nothing established.
 */
export type OriginalActionFacts = {
  originalActionAttempted?: boolean;
  originalActionPerformed?: boolean;
};

export function originalActionFacts(
  started: boolean,
  instructionGiven: boolean,
  attemptSucceeded: boolean,
): OriginalActionFacts {
  if (!started) {
    return { originalActionAttempted: false, originalActionPerformed: false };
  }
  if (!instructionGiven) {
    return {};
  }
  return { originalActionAttempted: true, originalActionPerformed: attemptSucceeded };
}

/**
 * The typed refusal for a FAILED scoped attempt that dispatched nothing, or
 * `null` when something was dispatched (then the answer is `RECOVERY_FAILED`).
 * The guard's own refusal wins; otherwise the executor missed before acting.
 */
export function undispatchedRefusal(
  record: RecoveryDispatchRecord,
  reason: string | null,
): { code: string; error: string } | null {
  if (record.dispatched) return null;
  if (record.refusal) return { code: record.refusal.code, error: record.refusal.message };
  return {
    code: RECOVERY_NOT_DISPATCHED,
    error: `${RECOVERY_NOT_DISPATCHED}: ${reason ?? "the recovery executor dispatched no action"}`,
  };
}
