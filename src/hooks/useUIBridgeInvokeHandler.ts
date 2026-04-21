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
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";

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
 * Install a Tauri listener on `ui-bridge:invoke-request` that dispatches to
 * `invoke(command, args)` and emits the result back over
 * `ui-bridge:invoke-response`.
 *
 * Safe to mount once at app root alongside `UIBridgeEventHandler`. Multiple
 * concurrent invokes are supported (each carries its own `request_id`).
 */
export function useUIBridgeInvokeHandler(): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        log.debug("Setting up ui-bridge:invoke-request listener");

        unlisten = await listen<InvokeRequestPayload>("ui-bridge:invoke-request", async (event) => {
          if (!isMounted) {
            log.debug("Component unmounted, ignoring invoke request");
            return;
          }

          const { request_id, command, args } = event.payload;
          log.debug(`Received invoke request: ${command}`, request_id);

          let response: InvokeResponsePayload;
          try {
            // Forward args verbatim. Tauri's IPC renames top-level
            // camelCase keys to snake_case for the Rust command signature.
            const result = await invoke(command, args ?? {});
            response = {
              request_id,
              ok: true,
              result: result as unknown,
            };
          } catch (err) {
            const message = getErrorMessage(err);
            log.debug(`invoke(${command}) threw:`, message);
            response = {
              request_id,
              ok: false,
              error: message,
            };
          }

          try {
            await emit("ui-bridge:invoke-response", response);
          } catch (emitErr) {
            // If emit itself fails, the Rust side will hit its timeout.
            // There's no better fallback here — we log and move on.
            console.error("[UIBridgeInvokeHandler] Failed to emit invoke-response:", emitErr);
          }
        });

        log.debug("invoke-request listener set up successfully");
      } catch (error) {
        console.error("[UIBridgeInvokeHandler] Failed to set up invoke-request listener:", error);
      }
    };

    setupListener();

    return () => {
      log.debug("Cleaning up invoke-request listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
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
