/**
 * useTerminalBufferIpc
 *
 * Frontend half of the `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer`
 * correlation flow. Mirrors `useUIBridgeEvaluateHandler` but for terminal
 * buffer readback.
 *
 * Architecture:
 * ```
 * External HTTP Client
 *     | GET /ui-bridge/sdk/terminal/sessions/:sid/buffer?lines=N
 *     v
 * Axum handler (mcp/sdk_terminal_buffer.rs::handle_terminal_buffer)
 *     | emit("ui-bridge:terminal-buffer-request",
 *     |      { request_id, session_id, max_lines })
 *     v
 * This Hook
 *     | lookupTerminalBufferReader(session_id) → reader(max_lines)
 *     v
 * This Hook
 *     | emit("ui-bridge:terminal-buffer-response",
 *     |      { request_id, ok, lines?, total_lines?, error? })
 *     v
 * Axum listener (mcp_api.rs) — delivers to the pending oneshot in
 * TerminalBufferRequestStore → HTTP handler returns the value.
 * ```
 *
 * The session-id-to-backend lookup goes through the module-level
 * `terminalBufferRegistry`, which TerminalInstance auto-populates on mount.
 * This decouples the IPC listener from React's per-page TerminalCoreContext
 * — terminals can live in any TerminalPage tree, and the global registry
 * surfaces them all under a single key.
 */

import { useEffect } from "react";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";
import { lookupTerminalBufferReader } from "@/lib/terminalBufferRegistry";

const log = createLogger("TerminalBufferIpc");

interface BufferRequestPayload {
  request_id: string;
  session_id: string;
  /** Window size cap. The Rust handler always sends a numeric cap (default
   *  `MAX_RETURNED_LINES`); a missing/zero value here means "all". */
  max_lines?: number;
}

interface BufferResponsePayload {
  request_id: string;
  ok: boolean;
  lines?: string[];
  total_lines?: number;
  error?: string;
}

/**
 * Install a Tauri listener on `ui-bridge:terminal-buffer-request` that looks
 * up the registered xterm reader for the given session_id, serializes the
 * buffer text, and emits the result back over
 * `ui-bridge:terminal-buffer-response`. Every request carries a `request_id`
 * which we echo verbatim so the Rust `TerminalBufferRequestStore` can
 * correlate concurrent callers.
 *
 * Safe to mount once at app root alongside `UIBridgeEvaluateHandler`.
 */
export function useTerminalBufferIpc(): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        log.debug("Setting up ui-bridge:terminal-buffer-request listener");

        unlisten = await listen<BufferRequestPayload>(
          "ui-bridge:terminal-buffer-request",
          async (event) => {
            if (!isMounted) {
              log.debug("Component unmounted, ignoring buffer request");
              return;
            }

            const { request_id, session_id, max_lines } = event.payload;
            log.debug(
              `Received terminal-buffer request ${request_id} for session ${session_id}`,
            );

            if (!request_id) {
              // Without a request_id we can't route the response back. Drop
              // the call entirely — the Rust side will observe a timeout.
              console.warn(
                "[TerminalBufferIpc] terminal-buffer-request missing request_id; ignoring",
              );
              return;
            }

            let response: BufferResponsePayload;

            if (typeof session_id !== "string" || session_id.length === 0) {
              response = {
                request_id,
                ok: false,
                error: "session_id is required",
              };
            } else {
              const reader = lookupTerminalBufferReader(session_id);
              if (!reader) {
                response = {
                  request_id,
                  ok: false,
                  error: "session_not_found",
                };
              } else {
                try {
                  // Treat 0 / negative / NaN as "no cap"; anything positive
                  // forwards verbatim to the reader so it returns a tail
                  // window without serializing the full scrollback.
                  const cap =
                    typeof max_lines === "number" && Number.isFinite(max_lines) && max_lines > 0
                      ? max_lines
                      : undefined;
                  const { lines, totalLines } = reader(cap);
                  response = {
                    request_id,
                    ok: true,
                    lines,
                    total_lines: totalLines,
                  };
                } catch (err) {
                  const message = getErrorMessage(err);
                  log.debug(`reader(${session_id}) threw:`, message);
                  response = {
                    request_id,
                    ok: false,
                    error: message,
                  };
                }
              }
            }

            try {
              await emit("ui-bridge:terminal-buffer-response", response);
            } catch (emitErr) {
              // If emit itself fails, the Rust side will hit its timeout.
              console.error(
                "[TerminalBufferIpc] Failed to emit terminal-buffer-response:",
                emitErr,
              );
            }
          },
        );

        log.debug("terminal-buffer-request listener set up successfully");
      } catch (error) {
        console.error(
          "[TerminalBufferIpc] Failed to set up terminal-buffer-request listener:",
          error,
        );
      }
    };

    setupListener();

    return () => {
      log.debug("Cleaning up terminal-buffer-request listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);
}

/**
 * Component wrapper for the hook (matches the `UIBridgeEvaluateHandler`
 * pattern so it can be placed alongside it in the JSX tree).
 */
export function TerminalBufferIpcHandler(): null {
  useTerminalBufferIpc();
  return null;
}
