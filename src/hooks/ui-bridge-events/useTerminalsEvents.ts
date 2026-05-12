/**
 * useTerminalsEvents
 *
 * Handles `terminal_sessions_list` and `terminal_session_get` IPC
 * requests issued by the new `GET /ui-bridge/control/terminal-sessions`
 * + `GET /ui-bridge/control/terminal-sessions/{id}` HTTP routes in the
 * Rust `mcp::ui_bridge::terminals` family.
 *
 * The hook reads from the module-level `terminal-sessions-registry`
 * that `TerminalPageInner` publishes on every render. The registry
 * scopes the response to the active terminal page; tabs from other
 * `pageId`s (multi-window setups) would publish a different snapshot
 * when they were on top, but the registry holds a single snapshot at
 * a time — matching `instanceStorage[ACTIVE_TAB_STORAGE_KEY]`'s
 * "current page wins" behaviour for `tabs_list`.
 *
 * The empty case (no terminal tabs open, or the page hasn't mounted
 * yet) returns `{sessions: []}` — NOT an error — so a caller polling
 * during runner startup gets a clean "nothing yet" signal.
 */

import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext, TerminalEventTypes } from "./types";
import { createLogger } from "@/lib/logger";
import {
  getTerminalSession,
  getTerminalSessions,
  type TerminalSessionEntry,
} from "@/lib/terminal-sessions-registry";

const logger = createLogger("UIBridgeTerminalsEvents");

const TERMINAL_EVENT_TYPES: ReadonlySet<TerminalEventTypes> = new Set<TerminalEventTypes>([
  "terminal_sessions_list",
  "terminal_session_get",
]);

function isTerminalEventType(type: string): type is TerminalEventTypes {
  return TERMINAL_EVENT_TYPES.has(type as TerminalEventTypes);
}

/**
 * Project a registry entry to the IPC wire shape. snake_case keys mirror
 * the Rust DTO conventions for `/control/*` payload bodies (`task_run_id`,
 * `working_dir`, `claude_session_id`, `created_at`) so downstream typed
 * consumers in qontinui-types can decode the response without an
 * additional rename layer. Exported for vitest.
 */
export function projectTerminalSessionEntry(entry: TerminalSessionEntry) {
  return {
    id: entry.id,
    title: entry.title,
    task_run_id: entry.taskRunId,
    claude_session_id: entry.claudeSessionId,
    working_dir: entry.workingDir,
    state: entry.state,
    is_alive: entry.isAlive,
    exit_code: entry.exitCode,
    type: entry.type,
    created_at: entry.createdAt,
  };
}

export function useTerminalsEvents(
  context: Pick<UIBridgeEventContext, "sendResponse">,
) {
  const { sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      if (!isTerminalEventType(type)) {
        return false;
      }

      switch (type) {
        case "terminal_sessions_list": {
          const sessions = getTerminalSessions().map(projectTerminalSessionEntry);
          logger.debug(`terminal_sessions_list: returning ${sessions.length} entries`);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { sessions },
            timestamp: Date.now(),
          });
          return true;
        }

        case "terminal_session_get": {
          // Path param is plumbed through `payload.id`; tolerate
          // `payload.params.id` too in case future callers wrap the
          // payload under `params` (the legacy convention for several
          // /control/* handlers).
          const rawId =
            (payload as { id?: unknown }).id ??
            (payload.params as { id?: unknown } | undefined)?.id;
          if (typeof rawId !== "string" || rawId.length === 0) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "id is required (non-empty string)",
              timestamp: Date.now(),
            });
            return true;
          }
          const entry = getTerminalSession(rawId);
          if (!entry) {
            const knownIds = getTerminalSessions().map((e) => e.id);
            // Surface as a successful IPC response with `found: false`
            // so the Rust handler can branch to HTTP 404 with the
            // known-ids hint body. Returning an IPC `error` here would
            // strip the `data` payload via `wrap_ipc_result`.
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { found: false, knownIds },
              timestamp: Date.now(),
            });
            return true;
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { found: true, session: projectTerminalSessionEntry(entry) },
            timestamp: Date.now(),
          });
          return true;
        }

        default: {
          // Exhaustiveness guard — keeps `TerminalEventTypes` and the
          // switch in lockstep at compile time. Adding a new variant
          // to `TerminalEventTypes` without a matching `case` here
          // fails the `never` check below.
          const _exhaustive: never = type;
          void _exhaustive;
          return false;
        }
      }
    },
    [sendResponse],
  );
}
