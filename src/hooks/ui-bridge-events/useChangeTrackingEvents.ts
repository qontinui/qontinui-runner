import { useCallback } from "react";
import { handleChangeTrackingCommand } from "../changeTrackingHandler";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

/**
 * Handles: save_bookmark, get_bookmark, delete_bookmark, list_bookmarks,
 *          diff_from_bookmark, execute_with_diff, wait_for_change, categorize_last_diff,
 *          scoped_diff, summarize_diff, structured_changes, enable_change_buffer,
 *          disable_change_buffer, drain_change_buffer, get_change_buffer_size
 */
export function useChangeTrackingEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse" | "changeTrackerRef">,
) {
  const { bridgeRef, sendResponse, changeTrackerRef } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "save_bookmark":
        case "get_bookmark":
        case "delete_bookmark":
        case "list_bookmarks":
        case "diff_from_bookmark":
        case "execute_with_diff":
        case "wait_for_change":
        case "categorize_last_diff":
        case "scoped_diff":
        case "summarize_diff":
        case "structured_changes":
        case "enable_change_buffer":
        case "disable_change_buffer":
        case "drain_change_buffer":
        case "get_change_buffer_size": {
          const {
            ChangeTracker,
            createSnapshotManager,
            analyzeStructuredChanges: analyzeStructured,
          } = await import("ui-bridge/ai");

          // Lazy-init ChangeTracker singleton
          if (!changeTrackerRef.current) {
            const manager = createSnapshotManager({});
            changeTrackerRef.current = new ChangeTracker(
              {
                idleDetector: null,
                createControlSnapshot: () => {
                  const snap = currentBridge.createSnapshot();
                  return {
                    timestamp: Date.now(),
                    elements: snap.elements.map((e) => ({
                      id: e.id,
                      type: e.type,
                      label: e.label ?? "",
                      actions: e.actions,
                      state: e.state,
                    })),
                    components: [],
                    workflows: [],
                    activeRuns: [],
                  };
                },
                refreshElements: () => {},
                snapshotManager: manager,
                executeElementAction: async (
                  id: string,
                  request: { action: string; params?: Record<string, unknown> },
                ) => {
                  const result = await currentBridge.executeAction(id, {
                    action: request.action,
                    params: request.params,
                  });
                  return result;
                },
                resolveScope: (scope: string) => {
                  const container = document.querySelector(scope);
                  if (!container) return null;
                  const ids = new Set<string>();
                  for (const el of currentBridge.elements) {
                    if (el.element && container.contains(el.element as Node)) {
                      ids.add(el.id);
                    }
                  }
                  return ids;
                },
                // Push-based change observation (allio-inspired):
                // Subscribe to registry element events for event-driven waitForChange.
                // Access via .registry (added to UseUIBridgeReturn) with fallback for older builds.
                subscribeChanges: (() => {
                  const bridge = currentBridge as {
                    registry?: { on?: (type: string, cb: () => void) => () => void };
                  };
                  const reg = bridge.registry;
                  if (!reg?.on) return undefined;
                  const onEvent = reg.on.bind(reg);
                  return (callback: (event: { type: string; timestamp: number }) => void) => {
                    const unsubs = [
                      onEvent("element:registered", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                      onEvent("element:unregistered", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                      onEvent("element:stateChanged", () =>
                        callback({ type: "snapshot:changed", timestamp: Date.now() }),
                      ),
                    ];
                    return () => unsubs.forEach((u) => u());
                  };
                })(),
              },
              {
                defaultSettleTimeout: 3000,
                defaultSettleMinStable: 300,
                defaultPollInterval: 200,
              },
            );
          }

          const ct = changeTrackerRef.current;
          const ctResult = await handleChangeTrackingCommand(
            ct,
            type,
            payload as unknown as Record<string, unknown>,
            {
              createSnapshot: () => currentBridge.createSnapshot(),
              createSnapshotManager,
              analyzeStructuredChanges: analyzeStructured,
            },
          );

          await sendResponse({
            requestId,
            type,
            success: true,
            data: ctResult,
            timestamp: Date.now(),
          });
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse, changeTrackerRef],
  );
}
