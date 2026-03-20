import { useCallback } from "react";
import { getGlobalSpecStore } from "ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

/**
 * Handles: get_console_errors, clear_console_errors, get_specs, get_spec,
 *          get_undo_state, get_element_state, get_forms
 */
export function useDebugInspectEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "get_console_errors": {
          const bridge = getUIBridgeGlobal();
          const capture = bridge?.consoleCapture as
            | {
                getSince: (ts: number) => unknown[];
                getRecent: (n?: number) => unknown[];
                clear: () => void;
              }
            | undefined;

          if (!capture) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { errors: [], count: 0, note: "ConsoleCapture not installed" },
              timestamp: Date.now(),
            });
            return true;
          }

          const since = payload.params?.since as number | undefined;
          const limit = payload.params?.limit as number | undefined;
          const errors = since ? capture.getSince(since) : capture.getRecent(limit ?? 50);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { errors, count: errors.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "clear_console_errors": {
          const bridge2 = getUIBridgeGlobal();
          const capture2 = bridge2?.consoleCapture as { clear: () => void } | undefined;

          if (capture2) {
            capture2.clear();
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { cleared: !!capture2 },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_specs": {
          const store = getGlobalSpecStore();
          const allConfigs = store.getAll();
          const specs: Array<{ specId: string; config: unknown }> = [];
          for (const [id, config] of allConfigs) {
            specs.push({ specId: id, config });
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { specs, count: specs.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_spec": {
          const { specId } = payload;
          if (!specId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "specId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const specConfig = getGlobalSpecStore().get(specId);
          if (!specConfig) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Spec not found: ${specId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { specId, config: specConfig },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_undo_state": {
          const uiBridgeGlobal = getUIBridgeGlobal();
          const undoTracker = uiBridgeGlobal?.undoTracker as
            | { getSnapshotUndoContext: () => unknown }
            | undefined;

          await sendResponse({
            requestId,
            type,
            success: true,
            data: undoTracker?.getSnapshotUndoContext() ?? {
              note: "UndoTracker not installed",
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_element_state": {
          const { elementId: stateId } = payload;
          if (!stateId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const stateEl = currentBridge.getElement(stateId);
          if (!stateEl) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found: ${stateId}`,
              timestamp: Date.now(),
            });
            return true;
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: stateEl.getState(),
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_forms": {
          const { discoverForms } = await import("ui-bridge/ai");
          const formElements = currentBridge.elements
            .filter((el) => ["input", "select", "textarea", "checkbox", "radio"].includes(el.type))
            .map((el) => ({
              id: el.id,
              element: el.element,
              type: el.type,
              label: el.label,
              getState: () => el.getState(),
            }));

          const formsResult = discoverForms(formElements);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: formsResult,
            timestamp: Date.now(),
          });
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse],
  );
}
