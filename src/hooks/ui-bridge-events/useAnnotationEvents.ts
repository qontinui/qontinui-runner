import { useCallback, useRef } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

/**
 * Handles: annotations_list, annotations_create, annotations_get,
 *          annotations_update, annotations_delete, annotations_coverage, annotations_export,
 *          annotations_import
 */
export function useAnnotationEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  // Lazy-init annotation store (persists across renders)
  const storeRef = useRef<InstanceType<
    typeof import("@qontinui/ui-bridge/annotations").AnnotationStore
  > | null>(null);

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      // Lazy-init the annotation store on first use
      if (!storeRef.current) {
        const { AnnotationStore } = await import("@qontinui/ui-bridge/annotations");
        storeRef.current = new AnnotationStore();
      }
      const store = storeRef.current;

      switch (type) {
        case "annotations_list": {
          const annotations = store.getAll();
          const count = Object.keys(annotations).length;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { annotations, count },
            timestamp: Date.now(),
          });
          return true;
        }

        case "annotations_create": {
          const elementId = (payload.params?.elementId as string) ?? "";
          const text = (payload.params?.text as string) ?? "";
          const annotationType = (payload.params?.type as string) ?? "note";
          store.set(elementId, {
            text,
            type: annotationType,
            createdAt: Date.now(),
            updatedAt: Date.now(),
          } as never);
          const annotation = store.get(elementId);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { elementId, ...annotation },
            timestamp: Date.now(),
          });
          return true;
        }

        case "annotations_get": {
          const id = (payload.params?.id as string) ?? "";
          const annotation = store.get(id);
          if (annotation) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { elementId: id, ...annotation },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Annotation not found: ${id}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "annotations_update": {
          const updateId = (payload.params?.id as string) ?? "";
          const existing = store.get(updateId);
          if (existing) {
            const updates = (payload.params?.updates as Record<string, unknown>) ?? {};
            store.set(updateId, { ...existing, ...updates, updatedAt: Date.now() } as never);
            const updated = store.get(updateId);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { elementId: updateId, ...updated },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Annotation not found: ${updateId}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "annotations_delete": {
          const deleteId = (payload.params?.id as string) ?? "";
          const existed = store.get(deleteId) !== undefined;
          store.delete(deleteId);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { deleted: existed },
            timestamp: Date.now(),
          });
          return true;
        }

        case "annotations_coverage": {
          const currentBridge = bridgeRef.current;
          if (!currentBridge) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "Bridge not initialized",
              timestamp: Date.now(),
            });
            return true;
          }
          const discovered = await currentBridge.discover({ includeHidden: false });
          const elementIds = discovered.elements.map((el: { id: string }) => el.id);
          const annotations = store.getAll();
          const annotatedIds = new Set(Object.keys(annotations));
          const annotatedCount = elementIds.filter((id: string) => annotatedIds.has(id)).length;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              totalElements: elementIds.length,
              annotatedElements: annotatedCount,
              coveragePercent:
                elementIds.length > 0 ? Math.round((annotatedCount / elementIds.length) * 100) : 0,
              annotatedIds: [...annotatedIds],
              unannotatedIds: elementIds.filter((id: string) => !annotatedIds.has(id)),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "annotations_export": {
          const allAnnotations = store.getAll();
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              version: "1.0.0",
              annotations: allAnnotations,
              metadata: {
                exportedAt: new Date().toISOString(),
                count: Object.keys(allAnnotations).length,
              },
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "annotations_import": {
          const importParams = payload.params ?? payload.body ?? {};
          const importData = importParams.annotations as
            | Record<string, { text?: string; type?: string; [key: string]: unknown }>
            | undefined;

          if (!importData || typeof importData !== "object") {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "annotations object is required",
              timestamp: Date.now(),
            });
            return true;
          }

          let importedCount = 0;
          const merge = (importParams.merge as boolean) ?? true;

          for (const [elementId, annotation] of Object.entries(importData)) {
            if (!merge) {
              // Overwrite mode: delete existing first
              store.delete(elementId);
            }
            store.set(elementId, {
              ...annotation,
              text: annotation.text ?? "",
              type: annotation.type ?? "note",
              createdAt: (annotation.createdAt as number) ?? Date.now(),
              updatedAt: (annotation.updatedAt as number) ?? Date.now(),
            } as never);
            importedCount++;
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              imported: importedCount,
              totalAfterImport: Object.keys(store.getAll()).length,
            },
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
