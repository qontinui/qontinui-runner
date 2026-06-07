/**
 * useUIBridgeInvokeHandler
 *
 * Phase 3I.1 — Frontend half of the UI Bridge invoke proxy.
 *
 * Architecture:
 * ```
 * External HTTP Client
 *     | POST /ui-bridge/invoke/<command> { args }
 *     v
 * Axum handler (ui_bridge_invoke_handlers.rs)
 *     | emit("ui-bridge:invoke-request", { request_id, command, args })
 *     v
 * This Hook
 *     | invoke(command, args)
 *     v
 * Tauri command (Rust)
 *     | returns Result<T, String>
 *     v
 * This Hook
 *     | emit("ui-bridge:invoke-response", { request_id, ok, result, error })
 *     v
 * Axum listener (mcp_api.rs) — delivers to the pending oneshot in
 * InvokeRequestStore → HTTP handler returns the value.
 * ```
 *
 * The Rust side gates on an allowlist (`UI_BRIDGE_COMMANDS` in
 * `src-tauri/src/ui_bridge_invoke.rs`) before emitting, so this hook will
 * only ever be asked to invoke commands the runner has explicitly curated.
 * We still forward the error string on failure so the HTTP caller sees the
 * real Tauri error.
 */

import { useEffect } from "react";
import { emit, type Event } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";
import { acquireSingletonListener } from "./ui-bridge-events/singleton-listener";
import { invokeRequestDedupe } from "./ui-bridge-events/request-dedupe";

/**
 * Serialize an invoke() rejection into a string the Rust side can forward.
 *
 * Tauri commands that return `Result<_, SomeStruct>` reject the invoke
 * promise with a plain JSON Object (e.g. `{ kind, message }`), which
 * `String(obj)` flattens to the useless `"[object Object]"`. Preserve the
 * original structure instead so HTTP callers and Rust logs see the real
 * error `kind` / `message` fields.
 */
function serializeInvokeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  if (err !== null && typeof err === "object") {
    try {
      return JSON.stringify(err);
    } catch {
      // Fall through to getErrorMessage for circular refs etc.
    }
  }
  return getErrorMessage(err);
}

const log = createLogger("UIBridgeInvokeHandler");

interface InvokeRequestPayload {
  request_id: string;
  command: string;
  args: Record<string, unknown>;
}

interface InvokeResponsePayload {
  request_id: string;
  ok: boolean;
  result?: unknown;
  error?: string;
}

/**
 * Dependencies for {@link handleInvokeRequest}, injected so the dispatch
 * logic is unit-testable without a live Tauri runtime.
 */
export interface InvokeHandlerDeps {
  invoke: (command: string, args: Record<string, unknown>) => Promise<unknown>;
  emit: (event: string, payload: unknown) => Promise<void>;
}

/**
 * Pure(-ish) handler for one `ui-bridge:invoke-request` event.
 *
 * Defense-in-depth EXACTLY-ONCE guard: the FIRST listener to see a given
 * `request_id` claims it via {@link invokeRequestDedupe} and proceeds;
 * any later listener (e.g. an accumulated duplicate) observes the claim
 * and drops the call. This caps execution at one per request_id even if
 * the singleton subscription guard is somehow bypassed.
 *
 * Returns `true` if this call executed the command (won the claim),
 * `false` if it was deduped/ignored — exposed for tests.
 */
export async function handleInvokeRequest(
  payload: InvokeRequestPayload,
  deps: InvokeHandlerDeps,
): Promise<boolean> {
  const { request_id, command, args } = payload;

  if (!request_id) {
    // Without a request_id we can't route the response back. Drop it.
    console.warn("[UIBridgeInvokeHandler] invoke-request missing request_id; ignoring");
    return false;
  }

  if (!invokeRequestDedupe.claim(request_id)) {
    log.debug(`invoke(${request_id}) already handled; ignoring duplicate`);
    return false;
  }

  log.debug(`Received invoke request: ${command}`, request_id);

  let response: InvokeResponsePayload;
  try {
    // Forward args verbatim. Tauri's IPC renames top-level camelCase keys
    // to snake_case for the Rust command signature.
    const result = await deps.invoke(command, args ?? {});
    response = { request_id, ok: true, result: result as unknown };
  } catch (err) {
    const message = serializeInvokeError(err);
    log.debug(`invoke(${command}) threw:`, message);
    response = { request_id, ok: false, error: message };
  }

  try {
    await deps.emit("ui-bridge:invoke-response", response);
  } catch (emitErr) {
    // If emit itself fails, the Rust side will hit its timeout. There's no
    // better fallback here — we log and move on.
    console.error("[UIBridgeInvokeHandler] Failed to emit invoke-response:", emitErr);
  }
  return true;
}

const invokeDeps: InvokeHandlerDeps = {
  invoke: (command, args) => invoke(command, args),
  emit: (event, payload) => emit(event, payload),
};

/**
 * Install a Tauri listener on `ui-bridge:invoke-request` that dispatches to
 * `invoke(command, args)` and emits the result back over
 * `ui-bridge:invoke-response`.
 *
 * The subscription is a SINGLETON per webview (see
 * `ui-bridge-events/singleton-listener.ts`): no matter how many times this
 * hook mounts — including React StrictMode's mount→unmount→remount, which
 * runs in production too — exactly one underlying Tauri listener exists.
 * That is the structural fix for the duplicate-execution bug; the
 * `request_id` dedupe in {@link handleInvokeRequest} is the
 * defense-in-depth backstop.
 *
 * Safe to mount once at app root alongside `UIBridgeEventHandler`. Multiple
 * concurrent invokes are supported (each carries its own `request_id`).
 */
export function useUIBridgeInvokeHandler(): void {
  useEffect(() => {
    log.debug("Acquiring singleton ui-bridge:invoke-request listener");
    const release = acquireSingletonListener<InvokeRequestPayload>(
      "ui-bridge:invoke-request",
      (event: Event<InvokeRequestPayload>) => {
        void handleInvokeRequest(event.payload, invokeDeps);
      },
    );
    return () => {
      log.debug("Releasing singleton ui-bridge:invoke-request listener");
      release();
    };
  }, []);
}

/**
 * Component wrapper for the hook (matches the UIBridgeEventHandler pattern
 * so it can be placed alongside it in the JSX tree).
 */
export function UIBridgeInvokeHandler(): null {
  useUIBridgeInvokeHandler();
  return null;
}
