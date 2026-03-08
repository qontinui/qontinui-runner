/**
 * useUIBridgeEventHandler
 *
 * React hook that listens for Tauri `ui-bridge-request` events from the Rust backend
 * and responds with UI Bridge data via `ui-bridge-response` events.
 *
 * This enables the Axum HTTP server to communicate with the React UI Bridge,
 * allowing external tools (like Claude Code) to interact with the React UI.
 *
 * Architecture:
 * ```
 * External HTTP Client (e.g., Claude Code)
 *     | HTTP request
 *     v
 * Axum Server (mcp_api.rs /ui-bridge/* routes)
 *     | Tauri emit("ui-bridge-request", payload)
 *     v
 * This Hook (useUIBridgeEventHandler)
 *     | Uses useUIBridge() to access registry, executor, etc.
 *     v
 * Tauri emit("ui-bridge-response", response)
 *     | (Rust side can listen for this or use oneshot channel)
 *     v
 * Axum Server -> HTTP Response
 * ```
 */

import { useEffect, useCallback, useRef } from "react";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import {
  useUIBridge,
  getGlobalSpecStore,
  getElementDesignData,
  captureResponsiveSnapshots,
  captureStateVariations,
  runStyleAudit,
} from "ui-bridge";
import type {
  RegisteredElement,
  RegisteredComponent,
  ElementIdentifier,
  ElementState,
  BridgeSnapshot,
  ElementDesignData,
  StyleGuideConfig,
  DesignRegistryLike,
  InteractionStateName,
} from "ui-bridge";

/**
 * Request types that can come from the Rust backend
 */
type UIBridgeRequestType =
  | "get_elements"
  | "get_element"
  | "execute_action"
  | "get_components"
  | "get_component"
  | "execute_component_action"
  | "discover"
  | "get_snapshot"
  | "get_console_errors"
  | "clear_console_errors"
  | "get_specs"
  | "get_spec"
  | "page_refresh"
  | "page_navigate"
  | "page_go_back"
  | "page_go_forward"
  | "design_get_snapshot"
  | "design_get_responsive"
  | "design_run_audit"
  | "design_load_style_guide"
  | "design_get_style_guide"
  | "design_clear_style_guide"
  | "design_get_element_styles"
  | "design_get_state_styles";

/**
 * Payload structure for UI Bridge requests from Rust
 */
interface UIBridgeRequestPayload {
  requestId: string;
  type: UIBridgeRequestType;
  elementId?: string;
  componentId?: string;
  actionId?: string;
  action?: {
    action: string;
    params?: Record<string, unknown>;
    waitOptions?: {
      visible?: boolean;
      enabled?: boolean;
      focused?: boolean;
      timeout?: number;
      interval?: number;
    };
  };
  specId?: string;
  url?: string;
  params?: Record<string, unknown>;
  guide?: StyleGuideConfig;
  elementIds?: string[];
  includePseudoElements?: boolean;
  viewports?: Record<string, number>;
  options?: {
    root?: string;
    interactiveOnly?: boolean;
    includeHidden?: boolean;
    limit?: number;
    types?: string[];
    selector?: string;
  };
}

/**
 * Response structure sent back to Rust
 */
interface UIBridgeResponsePayload {
  requestId: string;
  type: UIBridgeRequestType;
  success: boolean;
  data?: unknown;
  error?: string;
  timestamp: number;
}

/**
 * Element data formatted for the response (serializable version)
 */
interface SerializedElement {
  id: string;
  type: string;
  label?: string;
  actions: string[];
  customActions?: string[];
  identifier: ElementIdentifier;
  state: ElementState;
  registeredAt: number;
  mounted: boolean;
}

/**
 * Component data formatted for the response (serializable version)
 */
interface SerializedComponent {
  id: string;
  name: string;
  description?: string;
  actions: Array<{
    id: string;
    label?: string;
    description?: string;
  }>;
  elementIds?: string[];
  registeredAt: number;
  mounted: boolean;
}

/**
 * Serialize a RegisteredElement to a plain object (removes DOM references)
 */
function serializeElement(element: RegisteredElement): SerializedElement {
  return {
    id: element.id,
    type: element.type,
    label: element.label,
    actions: element.actions,
    customActions: element.customActions ? Object.keys(element.customActions) : undefined,
    identifier: element.getIdentifier(),
    state: element.getState(),
    registeredAt: element.registeredAt,
    mounted: element.mounted,
  };
}

/**
 * Serialize a RegisteredComponent to a plain object
 */
function serializeComponent(component: RegisteredComponent): SerializedComponent {
  return {
    id: component.id,
    name: component.name,
    description: component.description,
    actions: component.actions.map((a) => ({
      id: a.id,
      label: a.label,
      description: a.description,
    })),
    elementIds: component.elementIds,
    registeredAt: component.registeredAt,
    mounted: component.mounted,
  };
}

/**
 * Hook that handles UI Bridge requests from Tauri events.
 *
 * This hook should be used inside the UIBridgeProvider to have access
 * to the registry and executor.
 */
export function useUIBridgeEventHandler(): void {
  const bridge = useUIBridge();
  const bridgeRef = useRef(bridge);
  const loadedStyleGuideRef = useRef<StyleGuideConfig | null>(null);

  // Keep bridge ref updated to avoid stale closures
  useEffect(() => {
    bridgeRef.current = bridge;
  }, [bridge]);

  /**
   * Send a response back to the Rust backend
   */
  const sendResponse = useCallback(async (response: UIBridgeResponsePayload) => {
    try {
      await emit("ui-bridge-response", response);
      console.log(`[UIBridgeEventHandler] Sent response for ${response.type}:`, response.requestId);
    } catch (error) {
      console.error("[UIBridgeEventHandler] Failed to emit response:", error);
    }
  }, []);

  /**
   * Handle incoming UI Bridge requests
   */
  const handleRequest = useCallback(
    async (payload: UIBridgeRequestPayload) => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      console.log(`[UIBridgeEventHandler] Received request: ${type}`, requestId);

      // Check if UI Bridge is available
      if (!currentBridge.available) {
        await sendResponse({
          requestId,
          type,
          success: false,
          error: "UI Bridge is not available",
          timestamp: Date.now(),
        });
        return;
      }

      try {
        switch (type) {
          case "get_elements": {
            const elements = currentBridge.elements.map(serializeElement);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { elements, count: elements.length },
              timestamp: Date.now(),
            });
            break;
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
              return;
            }

            const element = currentBridge.getElement(elementId);
            if (!element) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `Element not found: ${elementId}`,
                timestamp: Date.now(),
              });
              return;
            }

            await sendResponse({
              requestId,
              type,
              success: true,
              data: serializeElement(element),
              timestamp: Date.now(),
            });
            break;
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
              return;
            }

            const result = await currentBridge.executeAction(elementId, {
              action: action.action,
              params: action.params,
              waitOptions: action.waitOptions,
            });

            await sendResponse({
              requestId,
              type,
              success: result.success,
              data: result,
              error: result.error,
              timestamp: Date.now(),
            });
            break;
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
            break;
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
              return;
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
              return;
            }

            await sendResponse({
              requestId,
              type,
              success: true,
              data: serializeComponent(component),
              timestamp: Date.now(),
            });
            break;
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
              return;
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
            break;
          }

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
            break;
          }

          case "get_snapshot": {
            const snapshot: BridgeSnapshot = await currentBridge.createSnapshotAsync();

            await sendResponse({
              requestId,
              type,
              success: true,
              data: snapshot,
              timestamp: Date.now(),
            });
            break;
          }

          case "get_console_errors": {
            const w = window as unknown as Record<string, unknown>;
            const bridge = w.__UI_BRIDGE__ as Record<string, unknown> | undefined;
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
              return;
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
            break;
          }

          case "clear_console_errors": {
            const w2 = window as unknown as Record<string, unknown>;
            const bridge2 = w2.__UI_BRIDGE__ as Record<string, unknown> | undefined;
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
            break;
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
            break;
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
              return;
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
              return;
            }

            await sendResponse({
              requestId,
              type,
              success: true,
              data: { specId, config: specConfig },
              timestamp: Date.now(),
            });
            break;
          }

          case "page_refresh": {
            window.location.reload();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url: window.location.href },
              timestamp: Date.now(),
            });
            break;
          }

          case "page_navigate": {
            const { url } = payload;
            if (!url) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "url is required",
                timestamp: Date.now(),
              });
              return;
            }
            window.location.href = url;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url },
              timestamp: Date.now(),
            });
            break;
          }

          case "page_go_back": {
            window.history.back();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url: window.location.href },
              timestamp: Date.now(),
            });
            break;
          }

          case "page_go_forward": {
            window.history.forward();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url: window.location.href },
              timestamp: Date.now(),
            });
            break;
          }

          case "design_get_snapshot": {
            const allElements = currentBridge.elements;
            const targetIds = payload.elementIds as string[] | undefined;

            const designData: ElementDesignData[] = [];

            if (allElements.length > 0) {
              // Use registered elements from bridge
              const filtered = targetIds
                ? allElements.filter((el) => targetIds.includes(el.id))
                : allElements;

              for (const el of filtered) {
                let domEl: HTMLElement | null =
                  el.element instanceof HTMLElement ? el.element : null;
                if (!domEl) {
                  const selector = el.getIdentifier?.()?.selector;
                  if (selector) {
                    domEl = document.querySelector<HTMLElement>(selector);
                  }
                }
                if (domEl) {
                  designData.push(
                    getElementDesignData(domEl, {
                      elementId: el.id,
                      label: el.label,
                      type: el.type,
                      includePseudoElements: payload.includePseudoElements,
                    }),
                  );
                }
              }
            } else {
              // Fallback: scan DOM for all data-ui-id elements
              const domElements = document.querySelectorAll<HTMLElement>("[data-ui-id]");
              for (const domEl of domElements) {
                const uiId = domEl.getAttribute("data-ui-id") ?? "";
                if (targetIds && !targetIds.includes(uiId)) continue;
                designData.push(
                  getElementDesignData(domEl, {
                    elementId: uiId,
                    includePseudoElements: payload.includePseudoElements,
                  }),
                );
              }
            }

            await sendResponse({
              requestId,
              type,
              success: true,
              data: { elements: designData, timestamp: Date.now() },
              timestamp: Date.now(),
            });
            break;
          }

          case "design_get_element_styles": {
            const { elementId: styleElementId } = payload;
            if (!styleElementId) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "elementId is required",
                timestamp: Date.now(),
              });
              return;
            }

            const styleElement = currentBridge.getElement(styleElementId);
            let styleDomEl: HTMLElement | null = null;
            if (styleElement) {
              styleDomEl =
                styleElement.element instanceof HTMLElement ? styleElement.element : null;
              if (!styleDomEl) {
                const sel = styleElement.getIdentifier?.()?.selector;
                if (sel) {
                  styleDomEl = document.querySelector<HTMLElement>(sel);
                }
              }
            }
            if (!styleDomEl) {
              // Try direct selector query as last resort
              styleDomEl = document.querySelector<HTMLElement>(`[data-ui-id="${styleElementId}"]`);
            }

            if (!styleDomEl) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `Element not found in DOM: ${styleElementId}`,
                timestamp: Date.now(),
              });
              return;
            }

            const elementDesignData = getElementDesignData(styleDomEl, {
              elementId: styleElementId,
              label: styleElement?.label,
              type: styleElement?.type,
              includePseudoElements: true,
            });

            await sendResponse({
              requestId,
              type,
              success: true,
              data: elementDesignData,
              timestamp: Date.now(),
            });
            break;
          }

          case "design_get_state_styles": {
            const { elementId: stateElementId } = payload;
            if (!stateElementId) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "elementId is required",
                timestamp: Date.now(),
              });
              return;
            }

            const stateElement = currentBridge.getElement(stateElementId);
            let stateDomEl: HTMLElement | null = null;
            if (stateElement) {
              stateDomEl =
                stateElement.element instanceof HTMLElement ? stateElement.element : null;
              if (!stateDomEl) {
                const sel = stateElement.getIdentifier?.()?.selector;
                if (sel) {
                  stateDomEl = document.querySelector<HTMLElement>(sel);
                }
              }
            }
            if (!stateDomEl) {
              stateDomEl = document.querySelector<HTMLElement>(`[data-ui-id="${stateElementId}"]`);
            }

            if (!stateDomEl) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `Element not found in DOM: ${stateElementId}`,
                timestamp: Date.now(),
              });
              return;
            }

            const states =
              (payload.params?.states as InteractionStateName[] | undefined) ?? undefined;
            const stateVariations = await captureStateVariations(stateDomEl, states);

            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                elementId: stateElementId,
                states: stateVariations,
              },
              timestamp: Date.now(),
            });
            break;
          }

          case "design_get_responsive": {
            const registry = {
              getAllElements: () => currentBridge.elements,
            };
            const viewports = (payload.viewports ??
              payload.params?.viewports ?? { mobile: 375, tablet: 768, desktop: 1280 }) as Record<
              string,
              number
            >;
            const snapshots = await captureResponsiveSnapshots(
              registry as DesignRegistryLike,
              viewports,
            );

            await sendResponse({
              requestId,
              type,
              success: true,
              data: snapshots,
              timestamp: Date.now(),
            });
            break;
          }

          case "design_run_audit": {
            const auditElements = currentBridge.elements;
            const auditTargetIds =
              payload.elementIds ?? (payload.params?.elementIds as string[] | undefined);

            const auditDesignData: ElementDesignData[] = [];
            if (auditElements.length > 0) {
              const auditFiltered = auditTargetIds
                ? auditElements.filter((el) => auditTargetIds.includes(el.id))
                : auditElements;
              for (const el of auditFiltered) {
                let domEl: HTMLElement | null =
                  el.element instanceof HTMLElement ? el.element : null;
                if (!domEl) {
                  const sel = el.getIdentifier?.()?.selector;
                  if (sel) {
                    domEl = document.querySelector<HTMLElement>(sel);
                  }
                }
                if (domEl) {
                  auditDesignData.push(
                    getElementDesignData(domEl, {
                      elementId: el.id,
                      label: el.label,
                      type: el.type,
                    }),
                  );
                }
              }
            } else {
              const domElements = document.querySelectorAll<HTMLElement>("[data-ui-id]");
              for (const domEl of domElements) {
                const uiId = domEl.getAttribute("data-ui-id") ?? "";
                if (auditTargetIds && !auditTargetIds.includes(uiId)) continue;
                auditDesignData.push(getElementDesignData(domEl, { elementId: uiId }));
              }
            }

            const guide =
              payload.guide ??
              (payload.params?.guide as StyleGuideConfig | undefined) ??
              loadedStyleGuideRef.current;
            if (!guide) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error:
                  "No style guide provided or loaded. Load one with design_load_style_guide first, or pass a guide in the request.",
                timestamp: Date.now(),
              });
              return;
            }

            const report = runStyleAudit(auditDesignData, guide);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: report,
              timestamp: Date.now(),
            });
            break;
          }

          case "design_load_style_guide": {
            const guideToLoad =
              payload.guide ?? (payload.params?.guide as StyleGuideConfig | undefined);
            if (!guideToLoad) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "guide is required",
                timestamp: Date.now(),
              });
              return;
            }
            loadedStyleGuideRef.current = guideToLoad;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { loaded: true },
              timestamp: Date.now(),
            });
            break;
          }

          case "design_get_style_guide": {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: loadedStyleGuideRef.current,
              timestamp: Date.now(),
            });
            break;
          }

          case "design_clear_style_guide": {
            loadedStyleGuideRef.current = null;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { cleared: true },
              timestamp: Date.now(),
            });
            break;
          }

          default: {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Unknown request type: ${type}`,
              timestamp: Date.now(),
            });
          }
        }
      } catch (error) {
        console.error(`[UIBridgeEventHandler] Error handling ${type}:`, error);
        await sendResponse({
          requestId,
          type,
          success: false,
          error: error instanceof Error ? error.message : String(error),
          timestamp: Date.now(),
        });
      }
    },
    [sendResponse],
  );

  // Set up the Tauri event listener
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        console.log("[UIBridgeEventHandler] Setting up ui-bridge-request listener");

        unlisten = await listen<UIBridgeRequestPayload>("ui-bridge-request", (event) => {
          if (!isMounted) {
            console.log("[UIBridgeEventHandler] Component unmounted, ignoring event");
            return;
          }

          const payload = event.payload;
          console.log("[UIBridgeEventHandler] Received event:", payload.type, payload.requestId);

          // Handle the request asynchronously
          handleRequest(payload).catch((error) => {
            console.error("[UIBridgeEventHandler] Unhandled error in request handler:", error);
          });
        });

        console.log("[UIBridgeEventHandler] Listener set up successfully");
      } catch (error) {
        console.error("[UIBridgeEventHandler] Failed to set up listener:", error);
      }
    };

    setupListener();

    return () => {
      console.log("[UIBridgeEventHandler] Cleaning up listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, [handleRequest]);
}

/**
 * Component wrapper for the hook (for easier usage in JSX)
 */
export function UIBridgeEventHandler(): null {
  useUIBridgeEventHandler();
  return null;
}
