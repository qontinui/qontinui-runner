import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import type { MainTabId } from "@/components/app/tab-types";
import { migrateTabId } from "@/components/app/tab-types";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { serializeElement, serializeComponent } from "./utils";

/**
 * Handles: get_elements, get_element, execute_action, get_components, get_component,
 *          execute_component_action, resolve_stable_ref, navigate_tab, clear_storage
 */
export function useControlEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "get_elements": {
          const snapshot = await currentBridge.createSnapshotAsync();
          const elements = snapshot.elements;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: elements,
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_element": {
          const { elementId } = payload;
          if (!elementId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          // Try registry lookup first
          const element = currentBridge.getElement(elementId);
          if (element) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: serializeElement(element),
              timestamp: Date.now(),
            });
            return true;
          }

          // Fallback: search discovered elements by ID
          // This handles auto-discovered elements not in the registry
          try {
            const discovered = await currentBridge.discover({ includeHidden: true });
            const match = discovered.elements.find((e) => e.id === elementId);
            if (match) {
              await sendResponse({
                requestId,
                type,
                success: true,
                data: match,
                timestamp: Date.now(),
              });
              return true;
            }
          } catch {
            // Discovery fallback failed — continue to error
          }

          await sendResponse({
            requestId,
            type,
            success: false,
            error: `Element not found: ${elementId}`,
            timestamp: Date.now(),
          });
          return true;
        }

        case "execute_action": {
          const { elementId, action } = payload;
          if (!elementId || !action) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId and action are required",
              timestamp: Date.now(),
            });
            return true;
          }

          // Normalize: action may be a string (from SDK proxy fallback)
          // or an object { action, params, waitOptions } (from control endpoint)
          const actionObj = typeof action === "string" ? { action } : action;

          const result = await currentBridge.executeAction(elementId, {
            action: actionObj.action,
            params: actionObj.params,
            waitOptions: actionObj.waitOptions,
          });

          await sendResponse({
            requestId,
            type,
            success: result.success,
            data: result,
            error:
              result.error ||
              (result.success === false
                ? `Action '${actionObj.action}' failed on element '${elementId}'`
                : undefined),
            timestamp: Date.now(),
          });

          // Capture render log entry for the interaction
          try {
            fetch(`http://localhost:${getApiPort()}/ui-bridge/control/render-log`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                type: "interaction",
                timestamp: Date.now(),
                action: actionObj?.action,
                elementId: elementId,
                elementType: currentBridge.getElement(elementId)?.type,
                success: result.success,
              }),
            }).catch(() => {});
          } catch {
            // Non-critical — don't block action execution
          }
          return true;
        }

        case "get_components": {
          const components = currentBridge.components.map(serializeComponent);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { components, count: components.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_component": {
          const { componentId } = payload;
          if (!componentId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "componentId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const component = currentBridge.getComponent(componentId);
          if (!component) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Component not found: ${componentId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: serializeComponent(component),
            timestamp: Date.now(),
          });
          return true;
        }

        case "execute_component_action": {
          const { componentId, actionId, params } = payload;
          if (!componentId || !actionId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "componentId and actionId are required",
              timestamp: Date.now(),
            });
            return true;
          }

          const result = await currentBridge.executeComponentAction(componentId, {
            action: actionId,
            params,
          });

          await sendResponse({
            requestId,
            type,
            success: result.success,
            data: result,
            error: result.error,
            timestamp: Date.now(),
          });
          return true;
        }

        case "navigate_tab": {
          const tab = (payload.params as Record<string, unknown> | undefined)?.tab as
            | string
            | undefined;
          if (!tab) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "params.tab is required",
              timestamp: Date.now(),
            });
            return true;
          }

          // Validate via migrateTabId (maps legacy names, validates against VALID_TAB_IDS)
          const resolved = migrateTabId(tab);

          // Use direct tab setter (bypasses PAGE_TO_TAB)
          window.dispatchEvent(
            new CustomEvent("ui-bridge-set-tab", {
              detail: { tab: resolved as MainTabId },
            }),
          );

          // Wait for navigation to settle
          await new Promise((r) => setTimeout(r, 500));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { navigatedTo: resolved },
            timestamp: Date.now(),
          });
          return true;
        }

        case "resolve_stable_ref": {
          const stableRef = (payload as unknown as Record<string, unknown>).stableRef;
          if (!stableRef) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "stableRef payload is required",
              timestamp: Date.now(),
            });
            return true;
          }

          try {
            const { resolveStableRef } = await import("ui-bridge/core");
            const resolved = resolveStableRef(stableRef as Parameters<typeof resolveStableRef>[0]);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: resolved
                ? { elementId: resolved.id, mounted: resolved.mounted }
                : { elementId: null },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "clear_storage": {
          // Clear all localStorage keys for the current instance port namespace
          const port = getApiPort();
          const keysToRemove: string[] = [];
          const otherPortPattern = /:\d+$/;
          for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            if (!key) continue;
            if (port === 9876) {
              // Default port: keys have no suffix. Only clear keys that don't
              // belong to another port (i.e., don't end with :<digits>).
              if (!otherPortPattern.test(key)) {
                keysToRemove.push(key);
              }
            } else {
              // Non-default port: keys end with :<port>
              if (key.endsWith(`:${port}`)) {
                keysToRemove.push(key);
              }
            }
          }
          for (const key of keysToRemove) {
            localStorage.removeItem(key);
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { cleared: keysToRemove.length, port },
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
