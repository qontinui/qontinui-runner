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
 *     | await Promise.resolve(result)
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
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";

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
}

interface EvaluateResponsePayload {
  request_id: string;
  ok: boolean;
  result?: unknown;
  error?: string;
}

/**
 * Block patterns that enable code injection, data exfiltration, or
 * persistent state corruption. Mirrors the allowlist used by the legacy
 * `page_evaluate` branch in `usePageEvents.ts` — see that file for the
 * rationale on each entry. Deliberately allows: localStorage/sessionStorage
 * (tab navigation), location.reload (page refresh after config changes),
 * globalThis/Reflect/Proxy (deep inspection).
 */
const DANGEROUS_PATTERNS: RegExp[] = [
  /\bimport\s*\(/,
  /\brequire\s*\(/,
  /\b__proto__\b/,
  /\bconstructor\s*\[/,
  /\beval\s*\(/,
  /\bnew\s+Function\b/,
  /\bfetch\s*\(/,
  /\bXMLHttpRequest\b/,
  /\bnavigator\.sendBeacon\b/,
  /\bWebSocket\b/,
  /\bdocument\.cookie\b/,
  /\bwindow\.open\b/,
  /\bwindow\.location\.(assign|replace)\b/,
  /\bwindow\.location\s*=/,
  /\blocation\.(assign|replace)\b/,
  /\blocation\s*=\s*["'`]/,
  /\bcrypto\.subtle\b/,
];

function rejectIfDangerous(expression: string): void {
  for (const pattern of DANGEROUS_PATTERNS) {
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
 * `let`/`const` + explicit `return`). Promises are awaited via
 * `Promise.resolve(...)` before the response is emitted.
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
  return await Promise.resolve(result);
}

/**
 * Install a Tauri listener on `ui-bridge:evaluate-request` that runs the
 * caller's expression (after security gating) and emits the result back
 * over `ui-bridge:evaluate-response`. Every request carries a `request_id`
 * which we echo verbatim so the Rust `EvaluateRequestStore` can correlate
 * concurrent callers.
 *
 * Safe to mount once at app root alongside `UIBridgeInvokeHandler`.
 */
export function useUIBridgeEvaluateHandler(): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        log.debug("Setting up ui-bridge:evaluate-request listener");

        unlisten = await listen<EvaluateRequestPayload>(
          "ui-bridge:evaluate-request",
          async (event) => {
            if (!isMounted) {
              log.debug("Component unmounted, ignoring evaluate request");
              return;
            }

            const { request_id, expression, unwrap } = event.payload;
            log.debug(`Received evaluate request`, request_id);

            let response: EvaluateResponsePayload;

            if (!request_id) {
              // Without a request_id we can't route the response back. Drop
              // the call entirely — the Rust side will observe a timeout.
              console.warn(
                "[UIBridgeEvaluateHandler] evaluate-request missing request_id; ignoring",
              );
              return;
            }

            if (typeof expression !== "string" || expression.length === 0) {
              response = {
                request_id,
                ok: false,
                error: "expression is required",
              };
            } else {
              try {
                rejectIfDangerous(expression);
                const resolved = await evaluateExpression(expression);
                if (unwrap === true) {
                  // Opt-in consistent shape: always `{ value, type }`.
                  // Mirrors the sibling unwrap branch in
                  // usePageEvents.ts::page_evaluate. Rust passes this
                  // shape through verbatim when unwrap=true so the HTTP
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
                    // Functions aren't structured-clone-safe. Surface the
                    // name (or `<anonymous>`) so the caller gets something
                    // useful.
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
                  // `tagged_page_evaluate` helper can pass the `data` field
                  // through unchanged: `{ result: object | { value: primitive } }`.
                  const resultField =
                    typeof resolved === "object" && resolved !== null
                      ? resolved
                      : { value: resolved };
                  response = {
                    request_id,
                    ok: true,
                    result: { success: true, result: resultField },
                  };
                }
              } catch (err) {
                const message = getErrorMessage(err);
                log.debug(`evaluate(${request_id}) threw:`, message);
                response = {
                  request_id,
                  ok: false,
                  error: message,
                };
              }
            }

            try {
              await emit("ui-bridge:evaluate-response", response);
            } catch (emitErr) {
              // If emit itself fails, the Rust side will hit its timeout.
              console.error("[UIBridgeEvaluateHandler] Failed to emit evaluate-response:", emitErr);
            }
          },
        );

        log.debug("evaluate-request listener set up successfully");
      } catch (error) {
        console.error(
          "[UIBridgeEvaluateHandler] Failed to set up evaluate-request listener:",
          error,
        );
      }
    };

    setupListener();

    return () => {
      log.debug("Cleaning up evaluate-request listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
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
