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
          // Extract options from nested payload.options or top-level payload fields
          const discoverSource = (payload.options ?? payload) as Record<string, unknown>;
          const discoverOptions: Record<string, unknown> = {};
          const discoverKeys = [
            "interactive_only",
            "interactiveOnly",
            "includeHidden",
            "include_hidden",
            "element_type",
            "types",
            "text",
            "role",
            "label",
            "selector",
            "limit",
          ];
          for (const key of discoverKeys) {
            if (discoverSource[key] !== undefined) {
              const mappedKey =
                key === "interactive_only"
                  ? "interactiveOnly"
                  : key === "include_hidden"
                    ? "includeHidden"
                    : key;
              discoverOptions[mappedKey] = discoverSource[key];
            }
          }
          const result = await currentBridge.discover(discoverOptions);

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
          // The Rust backend merges the HTTP request body at the top level
          // of the payload (alongside requestId and type). Extract known
          // FindRequest fields from the payload itself, falling back to
          // nested params/body for backward compatibility.
          const nested = payload.params ?? payload.body;
          const source = (nested && typeof nested === "object" ? nested : payload) as Record<
            string,
            unknown
          >;
          const findOptions: Record<string, unknown> = { includeHidden: true };
          const findKeys = [
            "element_type",
            "types",
            "text",
            "exact_text",
            "role",
            "label",
            "root",
            "selector",
            "interactiveOnly",
            "interactive_only",
            "includeContent",
            "contentOnly",
            "includeHidden",
            "limit",
          ];
          for (const key of findKeys) {
            if (source[key] !== undefined) {
              // Map snake_case interactive_only to camelCase interactiveOnly
              const mappedKey = key === "interactive_only" ? "interactiveOnly" : key;
              findOptions[mappedKey] = source[key];
            }
          }
          const discovered = await currentBridge.discover(findOptions);
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
