import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

/**
 * Handles: ai_search, ai_find
 */
export function useAISearchEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "ai_search": {
          const { SearchEngine } = await import("ui-bridge/ai");
          // Use discover() for fresh elements with up-to-date visibility state
          // (currentBridge.elements is memoized and may have stale DOM refs)
          const discovered = await currentBridge.discover({ includeHidden: true });
          const engine = new SearchEngine({ includeHidden: true });
          engine.updateElements(discovered.elements);
          const criteria = payload.params ?? payload.body ?? {};
          const results = engine.search(criteria as Parameters<typeof engine.search>[0]);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: results,
            timestamp: Date.now(),
          });
          return true;
        }

        case "ai_find": {
          const { SearchEngine, find: aiFindFn } = await import("ui-bridge/ai");
          const discovered = await currentBridge.discover({ includeHidden: true });
          const engine = new SearchEngine({ includeHidden: true });
          engine.updateElements(discovered.elements);

          const query = (payload.params?.query as string) ?? "";
          const context = payload.params?.context as Record<string, unknown> | undefined;
          const confidenceThreshold = payload.params?.confidenceThreshold as number | undefined;

          // Auto-detect active modal for context-aware scoring
          const uiBridgeGlobal = getUIBridgeGlobal();
          const modalDet = uiBridgeGlobal?.modalDetector as
            | { detect: () => { modals: Array<{ id: string }> } }
            | undefined;
          const findContext: Record<string, unknown> = { ...context };
          if (!findContext.activeModalId && modalDet) {
            try {
              const modalStack = modalDet.detect();
              if (modalStack.modals.length > 0) {
                findContext.activeModalId = modalStack.modals[modalStack.modals.length - 1].id;
              }
            } catch {
              /* ignore */
            }
          }

          const result = aiFindFn(query, engine, {
            context: findContext as Parameters<typeof aiFindFn>[2] extends
              | { context?: infer C }
              | undefined
              ? C
              : never,
            confidenceThreshold,
            pickFirst: true,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: result,
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
