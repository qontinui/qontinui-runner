import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext, GatedViewEventTypes } from "./types";
import { listOpenableViews, openView } from "../../lib/openable-views";

/**
 * Handles: list_openable_views, open_view
 *
 * The openable-view registry (P1/P4 of plan
 * 2026-07-08-ui-bridge-reach-and-verify-gated-flows) lives in the frontend —
 * only React knows which views are registered and how to mount them — so these
 * are IPC-bridge routes: the Rust handlers in
 * `src-tauri/src/mcp/ui_bridge/gated_flow.rs` funnel through
 * `ui_bridge_request_sync` and land here.
 *
 * `open_view` is additionally gated Rust-side by a capability flag
 * (`QONTINUI_UI_BRIDGE_VIEW_CONTROL=1`, off by default), so a released operator
 * build never reaches this handler for `open_view` at all. Registered openers
 * are non-destructive by contract — see `lib/openable-views.ts`.
 */
export function useGatedViewEvents(context: Pick<UIBridgeEventContext, "sendResponse">) {
  const { sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      switch (type as GatedViewEventTypes) {
        case "list_openable_views": {
          const views = listOpenableViews();
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { views, count: views.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "open_view": {
          const params = (payload.params ?? {}) as { name?: unknown };
          const name = typeof params.name === "string" ? params.name : "";

          if (!name) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "open_view requires a string `name`",
              timestamp: Date.now(),
            });
            return true;
          }

          const opened = await openView(name);
          if (!opened) {
            const known = listOpenableViews().map((v) => v.name);
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `no openable view named '${name}'. Registered views: ${
                known.length > 0 ? known.join(", ") : "(none)"
              }`,
              timestamp: Date.now(),
            });
            return true;
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            // The opener flips React state; the caller follows up with
            // discover/snapshot to see the now-mounted view.
            data: { opened: name },
            timestamp: Date.now(),
          });
          return true;
        }

        default:
          return false;
      }
    },
    [sendResponse],
  );
}
