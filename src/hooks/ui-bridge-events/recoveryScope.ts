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
 * 3. **Honest** — `recovered` is read off the executor's own verdict.
 *
 * A leaf module (zero imports), for the same reason `terminalWriteResult.ts`
 * is one: `useAISearchEvents` pulls React and the bridge, so nothing exported
 * from it is testable under the runner's `environment: "node"` vitest config.
 */

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

/**
 * Actions that MUTATE input state. Kept in lockstep with the runner-side
 * `is_write_action` in `src-tauri/src/mcp/ui_bridge/recovery_executor.rs`:
 * both ends refuse, so neither is the single point of failure.
 */
const WRITE_ACTIONS = new Set([
  "type",
  "settext",
  "setvalue",
  "fill",
  "input",
  "paste",
  "append",
  "clear",
  "select",
  "selectoption",
  "check",
  "uncheck",
  "toggle",
  "submit",
  "upload",
  "setfiles",
  "sendkeys",
  "presskey",
  "writetoterminal",
]);

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
 * Wrap a UI Bridge instance so every action it dispatches during recovery is
 * checked by {@link assertRecoveryDispatchAllowed} first. Component actions
 * and form fills are refused outright — neither is element-scoped, so neither
 * can be constrained to the addressed element.
 */
export function scopeRecoveryExecutor<T extends object>(bridge: T, targetElementId: string): T {
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
          assertRecoveryDispatchAllowed(targetElementId, elementId, action?.action ?? "");
          return (value as (...a: unknown[]) => unknown).call(target, elementId, action, ...rest);
        };
      }
      if (
        (prop === "executeComponentAction" || prop === "fillForm") &&
        typeof value === "function"
      ) {
        return async () => {
          throw new RecoveryRefusedError(
            RECOVERY_OUT_OF_SCOPE,
            `${RECOVERY_OUT_OF_SCOPE}: '${String(prop)}' is not element-scoped; recovery is limited to '${targetElementId}'.`,
          );
        };
      }
      return typeof value === "function"
        ? (value as (...a: unknown[]) => unknown).bind(target)
        : value;
    },
  }) as T;
}

/** The honest recovery verdict, read off the executor's own response. */
export interface RecoveryVerdict {
  /** `true` ONLY when the executor itself reported success. */
  recovered: boolean;
  /** The executor's failure reason, when it failed. */
  reason: string | null;
}

/**
 * Derive `recovered` from the executor's response instead of asserting it.
 *
 * The old handler hardcoded `recovered: true` on any resolved executor call,
 * so a recovery that found no element, fell below the confidence threshold, or
 * threw mid-action still reported success to the caller.
 */
export function recoveryVerdict(result: unknown): RecoveryVerdict {
  if (result === null || typeof result !== "object") {
    return { recovered: false, reason: "recovery executor returned no result" };
  }
  const r = result as { success?: unknown; error?: unknown; errorCode?: unknown };
  if (r.success === true) return { recovered: true, reason: null };
  const code = typeof r.errorCode === "string" ? r.errorCode : null;
  const message = typeof r.error === "string" ? r.error : "recovery did not succeed";
  return { recovered: false, reason: code ? `${code}: ${message}` : message };
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
 * What remains is the part that was never envelope duplication: `recovered`,
 * the verdict `as_recovery_failure` (`ai_analyze.rs`) keys its Ok-arm on, and
 * `code`, which stays alongside it so a data-only reader still sees the typed
 * token. Fields already present here win over the stamped ones, so setting
 * `recovered: true` from a failure path is not possible by accident.
 */
export function recoveryFailureData(
  code: string,
  extra: Record<string, unknown> = {},
): Record<string, unknown> {
  return { code, recovered: false, ...extra };
}
