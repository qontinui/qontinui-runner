import { useCallback } from "react";
import type { BridgeSnapshot } from "ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

/**
 * Handles: discover, find, get_snapshot
 */
export function useDiscoveryEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "discover": {
          const { options } = payload;
          const result = await currentBridge.discover(options);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: result,
            timestamp: Date.now(),
          });
          return true;
        }

        case "find": {
          const findParams = payload.params ?? payload.body ?? {};
          const discovered = await currentBridge.discover({
            ...(findParams as Record<string, unknown>),
            includeHidden: true,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: discovered,
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_snapshot": {
          console.log("[UIBridgeEventHandler] get_snapshot: creating snapshot...");
          const snapshot: BridgeSnapshot = await currentBridge.createSnapshotAsync();
          console.log(
            `[UIBridgeEventHandler] get_snapshot: snapshot created (${snapshot.elements.length} elements)`,
          );

          // Enrich with page context, modals, and toasts from trackers if available.
          // Each enrichment is wrapped individually so one failure doesn't break the snapshot.
          const uiBridgeGlobal = getUIBridgeGlobal();
          const elementPairs = currentBridge.elements.map((e) => ({
            id: e.id,
            element: e.element,
          }));

          // Safe getter: catches errors from any individual enrichment
          const safeGet = <T>(fn: () => T): T | undefined => {
            try {
              return fn();
            } catch (e) {
              console.warn("[UIBridgeEventHandler] Enrichment failed:", e);
              return undefined;
            }
          };

          const navTracker = uiBridgeGlobal?.navigationTracker as
            | { getSnapshotPageContext: () => unknown }
            | undefined;
          const modalDet = uiBridgeGlobal?.modalDetector as
            | { getSnapshotModalContext: () => unknown }
            | undefined;
          const toastCap = uiBridgeGlobal?.toastCapture as
            | { getSnapshotToastContext: () => unknown }
            | undefined;
          const relTracker = uiBridgeGlobal?.relationshipTracker as
            | {
                getSnapshotRelationshipContext: (
                  elements?: Array<{ id: string; element: Element }>,
                ) => unknown;
              }
            | undefined;
          const dndDetector = uiBridgeGlobal?.dragDropDetector as
            | {
                getSnapshotDragDropContext: (
                  elements?: Array<{ id: string; element: Element }>,
                ) => unknown;
              }
            | undefined;
          const undoTracker = uiBridgeGlobal?.undoTracker as
            | { getSnapshotUndoContext: () => unknown }
            | undefined;
          const shortcutTracker = uiBridgeGlobal?.shortcutTracker as
            | { getSnapshotShortcutContext: () => unknown }
            | undefined;

          console.log("[UIBridgeEventHandler] get_snapshot: enriching...");
          const enrichedSnapshot = {
            ...snapshot,
            page: safeGet(() => navTracker?.getSnapshotPageContext()),
            modalStack: safeGet(() => modalDet?.getSnapshotModalContext()),
            toasts: safeGet(() => toastCap?.getSnapshotToastContext()),
            relationships: safeGet(() => relTracker?.getSnapshotRelationshipContext(elementPairs)),
            dragDrop: safeGet(() => dndDetector?.getSnapshotDragDropContext(elementPairs)),
            undoRedo: safeGet(() => undoTracker?.getSnapshotUndoContext()),
            shortcuts: safeGet(() => shortcutTracker?.getSnapshotShortcutContext()),
          };

          const response = {
            requestId,
            type,
            success: true,
            data: enrichedSnapshot,
            timestamp: Date.now(),
          };

          await sendResponse(response);
          console.log(
            `[UIBridgeEventHandler] get_snapshot: response sent (${enrichedSnapshot.elements.length} elements)`,
          );
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse],
  );
}
