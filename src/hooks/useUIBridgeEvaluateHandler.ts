/**
 * useUIBridgeEvaluateHandler
 *
 * Plan item D (post-Phase-3J) — Frontend half of the tagged
 * `/control/page/evaluate` correlation flow.
 *
 * Architecture:
 * ```
 * External HTTP Client
 *     | POST /ui-bridge/control/page/evaluate { expression }
 *     v
 * Axum handler (mcp/ui_bridge.rs::page_evaluate_inner → tagged_page_evaluate)
 *     | emit("ui-bridge:evaluate-request", { request_id, expression, await_promise, timeout_ms })
 *     v
 * This Hook
 *     | new Function("return " + expression)()  (security-gated)
 *     | awaitWithTimeout(result, PAGE_EVALUATE_PROMISE_TIMEOUT_MS)
 *     v
 * This Hook
 *     | emit("ui-bridge:evaluate-response", { request_id, ok, result, error })
 *     v
 * Axum listener (mcp_api.rs) — delivers to the pending oneshot in
 * EvaluateRequestStore → HTTP handler returns the value.
 * ```
 *
 * Why a separate hook (instead of extending the legacy `usePageEvents`
 * page_evaluate branch)?
 *
 * The legacy `ui-bridge-request` / `ui-bridge-response` channel multiplexes
 * many unrelated request types through a single Tauri listener. Mixing a
 * concurrency-sensitive flow like `/page/evaluate` into that channel made
 * per-call correlation implicit — "it works because only one call is in
 * flight at a time" — which is a latent bug. Plan item D carves out a
 * dedicated event pair (`ui-bridge:evaluate-request` / `-response`) so
 * concurrent HTTP callers never observe each other's results.
 *
 * SECURITY: The same expression-pattern allowlist used by the legacy
 * `page_evaluate` handler in `usePageEvents.ts` is applied here. Any change
 * to one handler's blocklist MUST be mirrored in the other.
 */

import { useEffect } from "react";
import { emit, type Event } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";
import { awaitWithTimeout, PAGE_EVALUATE_PROMISE_TIMEOUT_MS } from "./ui-bridge-events/utils";
import { acquireSingletonListener } from "./ui-bridge-events/singleton-listener";
import { evaluateRequestDedupe } from "./ui-bridge-events/request-dedupe";

const log = createLogger("UIBridgeEvaluateHandler");

interface EvaluateRequestPayload {
  request_id: string;
  expression: string;
  await_promise?: boolean;
  timeout_ms?: number;
  /**
   * When true, emit the discriminated `{value, type}` shape instead of
   * the legacy `{success, result}` envelope. Matches the sibling unwrap
   * branch in usePageEvents.ts::page_evaluate.
   */
  unwrap?: boolean;
  /**
   * EXPLICIT opt-in. When true, relaxes only the four network-related
   * blocks (fetch / XMLHttpRequest / sendBeacon / WebSocket) so test
   * assertions can hit runner APIs directly. Every structural code-
   * injection block stays in force regardless. Mirrors the sibling
   * branch in usePageEvents.ts::page_evaluate.
   */
  allow_network_requests?: boolean;
}

interface EvaluateResponsePayload {
  request_id: string;
  ok: boolean;
  result?: unknown;
  error?: string;
}

/**
 * Structural code-injection blocks — always enforced. Mirrors the
 * structural set in `usePageEvents.ts::page_evaluate`. See that file for
 * per-entry rationale. Deliberately allows: localStorage/sessionStorage
 * (tab navigation), location.reload (page refresh after config changes),
 * globalThis/Reflect/Proxy (deep inspection).
 */
const STRUCTURAL_PATTERNS: RegExp[] = [
  /\bimport\s*\(/,
  /\brequire\s*\(/,
  /\b__proto__\b/,
  /\bconstructor\s*\[/,
  /\beval\s*\(/,
  /\bnew\s+Function\b/,
  /\bdocument\.cookie\b/,
  /\bwindow\.open\b/,
  /\bwindow\.location\.(assign|replace)\b/,
  /\bwindow\.location\s*=/,
  /\blocation\.(assign|replace)\b/,
  /\blocation\s*=\s*["'`]/,
  /\bcrypto\.subtle\b/,
];

/**
 * Network-related blocks — relaxed when the caller passes
 * `allowNetworkRequests: true` so test assertions can hit runner APIs
 * directly without the brittle `window["fet"+"ch"]` workaround.
 */
const NETWORK_PATTERNS: RegExp[] = [
  /\bfetch\s*\(/,
  /\bXMLHttpRequest\b/,
  /\bnavigator\.sendBeacon\b/,
  /\bWebSocket\b/,
];

function rejectIfDangerous(expression: string, allowNetworkRequests: boolean): void {
  const patterns = allowNetworkRequests
    ? STRUCTURAL_PATTERNS
    : [...STRUCTURAL_PATTERNS, ...NETWORK_PATTERNS];
  for (const pattern of patterns) {
    if (pattern.test(expression)) {
      throw new Error(`Expression rejected: contains prohibited pattern (${pattern.source})`);
    }
  }
}

/**
 * Evaluate a caller-supplied expression with the legacy page_evaluate
 * semantics: default-wrap as `return <expr>` so `document.title`-style
 * simple expressions evaluate to their value; fall back to raw function
 * body if the wrap produces a SyntaxError (e.g. caller wrote top-level
 * `let`/`const` + explicit `return`). Top-level Promises (and any
 * thenable) are auto-awaited with a {@link PAGE_EVALUATE_PROMISE_TIMEOUT_MS}
 * cap so `(async () => ...)()` and `Promise.resolve(...)` patterns return
 * their resolved value rather than the bare Promise object (which would
 * serialize to `{}`). Mirrors the sibling
 * `usePageEvents.ts::page_evaluate` branch.
 */
async function evaluateExpression(expression: string): Promise<unknown> {
  let result: unknown;
  try {
    result = new Function("return " + expression)();
  } catch (firstErr) {
    if (firstErr instanceof SyntaxError) {
      result = new Function(expression)();
    } else {
      throw firstErr;
    }
  }
  return await awaitWithTimeout(result, PAGE_EVALUATE_PROMISE_TIMEOUT_MS);
}

/**
 * Dependencies for {@link handleEvaluateRequest}, injected so the
 * evaluation logic is unit-testable without a live Tauri runtime.
 */
export interface EvaluateHandlerDeps {
  emit: (event: string, payload: unknown) => Promise<void>;
}

/**
 * Pure(-ish) handler for one `ui-bridge:evaluate-request` event.
 *
 * Defense-in-depth EXACTLY-ONCE guard: the FIRST listener to see a given
 * `request_id` claims it via {@link evaluateRequestDedupe} and proceeds;
 * any later listener (e.g. an accumulated duplicate) observes the claim
 * and drops the call. This caps execution at one per request_id even if
 * the singleton subscription guard is somehow bypassed — critical here
 * because the expression is executed for side effects (it can call
 * `window.__TAURI__.core.invoke(...)`).
 *
 * Returns `true` if this call executed the expression (won the claim),
 * `false` if it was deduped/ignored — exposed for tests.
 */
export async function handleEvaluateRequest(
  payload: EvaluateRequestPayload,
  deps: EvaluateHandlerDeps,
): Promise<boolean> {
  const { request_id, expression, unwrap, allow_network_requests } = payload;

  if (!request_id) {
    // Without a request_id we can't route the response back. Drop the call
    // entirely — the Rust side will observe a timeout.
    console.warn("[UIBridgeEvaluateHandler] evaluate-request missing request_id; ignoring");
    return false;
  }

  if (!evaluateRequestDedupe.claim(request_id)) {
    log.debug(`evaluate(${request_id}) already handled; ignoring duplicate`);
    return false;
  }

  log.debug(`Received evaluate request`, request_id);

  let response: EvaluateResponsePayload;

  if (typeof expression !== "string" || expression.length === 0) {
    response = { request_id, ok: false, error: "expression is required" };
  } else {
    try {
      rejectIfDangerous(expression, allow_network_requests === true);
      const resolved = await evaluateExpression(expression);
      if (unwrap === true) {
        // Opt-in consistent shape: always `{ value, type }`. Mirrors the
        // sibling unwrap branch in usePageEvents.ts::page_evaluate. Rust
        // passes this shape through verbatim when unwrap=true so the HTTP
        // caller sees `{success: true, data: {value, type}}`.
        let valueType: "scalar" | "object" | "undefined" | "function" | "null";
        let normalizedValue: unknown;
        if (resolved === null) {
          valueType = "null";
          normalizedValue = null;
        } else if (resolved === undefined) {
          valueType = "undefined";
          normalizedValue = undefined;
        } else if (typeof resolved === "function") {
          valueType = "function";
          // Functions aren't structured-clone-safe. Surface the name (or
          // `<anonymous>`) so the caller gets something useful.
          normalizedValue = (resolved as { name?: string }).name || "<anonymous>";
        } else if (typeof resolved === "object") {
          valueType = "object";
          normalizedValue = resolved;
        } else {
          valueType = "scalar";
          normalizedValue = resolved;
        }
        response = {
          request_id,
          ok: true,
          result: { value: normalizedValue, type: valueType },
        };
      } else {
        // Match the legacy IPC page_evaluate shape so the Rust
        // `tagged_page_evaluate` helper can pass the `data` field through
        // unchanged: `{ result: object | { value: primitive } }`.
        const resultField =
          typeof resolved === "object" && resolved !== null ? resolved : { value: resolved };
        response = {
          request_id,
          ok: true,
          result: { success: true, result: resultField },
        };
      }
    } catch (err) {
      const message = getErrorMessage(err);
      log.debug(`evaluate(${request_id}) threw:`, message);
      response = { request_id, ok: false, error: message };
    }
  }

  try {
    await deps.emit("ui-bridge:evaluate-response", response);
  } catch (emitErr) {
    // If emit itself fails, the Rust side will hit its timeout.
    console.error("[UIBridgeEvaluateHandler] Failed to emit evaluate-response:", emitErr);
  }
  return true;
}

const evaluateDeps: EvaluateHandlerDeps = {
  emit: (event, payload) => emit(event, payload),
};

/**
 * Install a Tauri listener on `ui-bridge:evaluate-request` that runs the
 * caller's expression (after security gating) and emits the result back
 * over `ui-bridge:evaluate-response`. Every request carries a `request_id`
 * which we echo verbatim so the Rust `EvaluateRequestStore` can correlate
 * concurrent callers.
 *
 * The subscription is a SINGLETON per webview (see
 * `ui-bridge-events/singleton-listener.ts`): no matter how many times this
 * hook mounts — including React StrictMode's mount→unmount→remount, which
 * runs in production too — exactly one underlying Tauri listener exists.
 * That is the structural fix for the duplicate-execution bug; the
 * `request_id` dedupe in {@link handleEvaluateRequest} is the
 * defense-in-depth backstop.
 *
 * Safe to mount once at app root alongside `UIBridgeInvokeHandler`.
 */
export function useUIBridgeEvaluateHandler(): void {
  useEffect(() => {
    log.debug("Acquiring singleton ui-bridge:evaluate-request listener");
    const release = acquireSingletonListener<EvaluateRequestPayload>(
      "ui-bridge:evaluate-request",
      (event: Event<EvaluateRequestPayload>) => {
        void handleEvaluateRequest(event.payload, evaluateDeps);
      },
    );
    return () => {
      log.debug("Releasing singleton ui-bridge:evaluate-request listener");
      release();
    };
  }, []);
}

/**
 * Component wrapper for the hook (matches the `UIBridgeInvokeHandler`
 * pattern so it can be placed alongside it in the JSX tree).
 */
export function UIBridgeEvaluateHandler(): null {
  useUIBridgeEvaluateHandler();
  return null;
}
