import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { serializeElement, serializeComponent } from "./utils";

/**
 * Handles: get_elements, get_element, execute_action, get_components, get_component,
 *          execute_component_action, navigate_tab, clear_storage
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

          // Dispatch navigation event — useAppNavigation listens for this
          // It resolves via PAGE_TO_TAB, or falls back to direct tab ID
          window.dispatchEvent(
            new CustomEvent("ui-bridge-navigate", { detail: { page: tab } }),
          );

          // Wait for navigation to settle
          await new Promise((r) => setTimeout(r, 500));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { navigatedTo: tab },
            timestamp: Date.now(),
          });
          return true;
        }

        case "clear_storage": {
          // Clear all localStorage keys for the current instance port namespace
          const port = getApiPort();
          const suffix = port === 9876 ? "" : `:${port}`;
          const keysToRemove: string[] = [];
          for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            if (key && (suffix === "" || key.endsWith(suffix))) {
              keysToRemove.push(key);
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
