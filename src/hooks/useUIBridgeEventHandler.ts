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
import { handleChangeTrackingCommand } from "./changeTrackingHandler";
import { getApiPort } from "@/lib/runner-api";

/**
 * Send a response to the Rust backend via HTTP fallback.
 * Used when the Tauri event system is unresponsive.
 */
async function httpSendResponse(response: unknown): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/ipc-response`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(response),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Send a pong to the Rust backend via HTTP fallback.
 */
async function httpSendPong(): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/pong`, {
      method: "POST",
    });
    return resp.ok;
  } catch {
    return false;
  }
}
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
  | "design_get_state_styles"
  | "query_selector"
  | "page_evaluate"
  // Change tracking
  | "save_bookmark"
  | "get_bookmark"
  | "delete_bookmark"
  | "list_bookmarks"
  | "diff_from_bookmark"
  | "execute_with_diff"
  | "wait_for_change"
  | "categorize_last_diff"
  | "scoped_diff"
  | "summarize_diff"
  | "structured_changes"
  | "enable_change_buffer"
  | "disable_change_buffer"
  | "drain_change_buffer"
  | "get_change_buffer_size"
  // Undo/Redo
  | "get_undo_state"
  // Forms
  | "get_forms"
  // Network requests
  | "get_network_requests"
  | "get_network_requests_in_flight"
  // Idle detection
  | "get_idle_status"
  | "wait_for_idle"
  | "diagnose_stuck_screen"
  // Keyboard shortcuts
  | "get_keyboard_shortcuts"
  // AI search & find
  | "ai_search"
  | "ai_find"
  | "find"
  | "get_workflows"
  | "get_element_state"
  // Workflow execution
  | "run_workflow"
  | "get_workflow_status"
  // Media discovery
  | "find_media"
  | "media_audit"
  | "capture_media_snapshot"
  | "analyze_media";

/**
 * Map task run status strings from the runner to SDK-compatible workflow status values.
 */
function mapTaskRunStatus(status: string): string {
  switch (status) {
    case "in_progress":
    case "running":
      return "running";
    case "completed":
    case "complete":
    case "success":
      return "completed";
    case "failed":
    case "error":
      return "failed";
    case "cancelled":
    case "stopped":
      return "cancelled";
    default:
      return "pending";
  }
}

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
  /** CSS selector for query_selector requests */
  selector?: string;
  /** JavaScript expression for page_evaluate requests */
  expression?: string;
  /** Element index for query_selector action targeting */
  index?: number;
  guide?: StyleGuideConfig;
  elementIds?: string[];
  includePseudoElements?: boolean;
  viewports?: Record<string, number>;
  /** Bookmark name for change tracking */
  name?: string;
  /** Full request body for change tracking commands */
  body?: Record<string, unknown>;
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
 * Typed accessor for the global `window.__UI_BRIDGE__` object.
 *
 * Centralises the repeated cast so callers get a typed record (or undefined)
 * without duplicating the `as unknown as Record<…>` dance everywhere.
 */
function getUIBridgeGlobal(): Record<string, unknown> | undefined {
  return (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | Record<string, unknown>
    | undefined;
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
  const changeTrackerRef = useRef<InstanceType<typeof import("ui-bridge/ai").ChangeTracker> | null>(
    null,
  );
  const networkTrackerRef = useRef<InstanceType<
    typeof import("ui-bridge").NetworkRequestTracker
  > | null>(null);
  const idleDetectorRef = useRef<InstanceType<
    typeof import("ui-bridge").CompositeIdleDetector
  > | null>(null);

  // Keep bridge ref updated synchronously during render to avoid stale closures.
  // Using useEffect would leave a gap between rerender and effect execution
  // where IPC events could arrive and use the stale bridge reference.
  bridgeRef.current = bridge;

  /**
   * Send a response back to the Rust backend
   */
  const sendResponse = useCallback(async (response: UIBridgeResponsePayload) => {
    // Send via both Tauri emit AND HTTP fallback simultaneously.
    // Tauri emit may hang if the IPC channel is broken (WebView2 issue),
    // so the HTTP fallback ensures the response always reaches the Rust backend.
    const httpPromise = httpSendResponse(response).then((ok) => {
      if (ok) {
        console.log(
          `[UIBridgeEventHandler] HTTP sent response for ${response.type}:`,
          response.requestId,
        );
      }
    });
    const emitPromise = emit("ui-bridge-response", response)
      .then(() => {
        console.log(
          `[UIBridgeEventHandler] Tauri emit sent response for ${response.type}:`,
          response.requestId,
        );
      })
      .catch(() => {
        // Tauri emit failed — HTTP fallback already running
      });
    await Promise.allSettled([httpPromise, emitPromise]);
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
            const snapshot = await currentBridge.createSnapshotAsync();
            const elements = snapshot.elements;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: elements,
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
              error: result.error || (result.success === false ? `Action '${actionObj.action}' failed on element '${elementId}'` : undefined),
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

            // Enrich with page context, modals, and toasts from trackers if available
            const uiBridgeGlobal = getUIBridgeGlobal();
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
            const elementPairs = currentBridge.elements.map((e) => ({
              id: e.id,
              element: e.element,
            }));
            const enrichedSnapshot = {
              ...snapshot,
              page: navTracker?.getSnapshotPageContext(),
              modalStack: modalDet?.getSnapshotModalContext(),
              toasts: toastCap?.getSnapshotToastContext(),
              relationships: relTracker?.getSnapshotRelationshipContext(elementPairs),
              dragDrop: dndDetector?.getSnapshotDragDropContext(elementPairs),
              undoRedo: undoTracker?.getSnapshotUndoContext(),
              shortcuts: shortcutTracker?.getSnapshotShortcutContext(),
            };

            await sendResponse({
              requestId,
              type,
              success: true,
              data: enrichedSnapshot,
              timestamp: Date.now(),
            });
            break;
          }

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
            // Do NOT call window.location.reload() — the runner is a Tauri app and
            // a full page reload resets all React state (auth, execution, terminals),
            // causing the "Checking authentication..." screen to flash repeatedly.
            console.log("[UIBridge] page_refresh: ignoring (full reload disabled in runner)");
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
            try {
              // Only allow relative URL navigation via history API.
              // Absolute URLs (window.location.href = ...) would cause a full page reload,
              // resetting all React state and flashing "Checking authentication...".
              if (url.startsWith("/")) {
                window.history.pushState({}, "", url);
                window.dispatchEvent(new PopStateEvent("popstate"));
              } else {
                console.warn(`[UIBridge] page_navigate: ignoring absolute URL navigation in runner: ${url}`);
              }
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { success: true, url },
                timestamp: Date.now(),
              });
            } catch (err) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `Navigation failed: ${err instanceof Error ? err.message : String(err)}`,
                timestamp: Date.now(),
              });
            }
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

          case "query_selector": {
            const { selector, index: selectorIndex } = payload;
            // action field is typed as object for execute_action, but for
            // query_selector the Rust side sends it as a plain string in params
            const selectorAction = (payload.params?.action as string) ?? undefined;
            if (!selector) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "selector is required",
                timestamp: Date.now(),
              });
              return;
            }
            try {
              const elements = document.querySelectorAll(selector);
              const results = Array.from(elements).map((el, i) => {
                const htmlEl = el as HTMLElement;
                const rect = htmlEl.getBoundingClientRect();
                return {
                  index: i,
                  tagName: htmlEl.tagName.toLowerCase(),
                  textContent: (htmlEl.textContent ?? "").slice(0, 200),
                  id: htmlEl.id || undefined,
                  className: htmlEl.className || undefined,
                  visible: rect.width > 0 && rect.height > 0,
                  rect: {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                  },
                };
              });

              // Optionally perform an action on a matched element
              if (selectorAction === "click") {
                const targetIdx = typeof selectorIndex === "number" ? selectorIndex : 0;
                const target = elements[targetIdx] as HTMLElement | undefined;
                if (target) {
                  target.click();
                }
              }

              await sendResponse({
                requestId,
                type,
                success: true,
                data: { count: results.length, elements: results },
                timestamp: Date.now(),
              });
            } catch (err) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: String(err),
                timestamp: Date.now(),
              });
            }
            break;
          }

          case "page_evaluate": {
            const { expression } = payload;
            if (!expression) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "expression is required",
                timestamp: Date.now(),
              });
              return;
            }
            try {
              // SECURITY: Avoid eval() on arbitrary expressions from event payloads.
              // Use new Function() which does not have access to the local scope,
              // and validate that the expression only contains safe property access
              // patterns (alphanumeric, dots, brackets) to mitigate injection risk.
              const safeExpressionPattern = /^[a-zA-Z_$][\w$.[\]'"()\s,]*$/;
              if (!safeExpressionPattern.test(expression)) {
                throw new Error(
                  "Expression rejected: only simple property access and function calls are allowed",
                );
              }
              const result = new Function("return " + expression)();
              const resolvedResult = await Promise.resolve(result);
              await sendResponse({
                requestId,
                type,
                success: true,
                data: {
                  result:
                    typeof resolvedResult === "object" ? resolvedResult : { value: resolvedResult },
                },
                timestamp: Date.now(),
              });
            } catch (err) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: String(err),
                timestamp: Date.now(),
              });
            }
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
              // No registered elements available — nothing to report
              console.log(
                "[UIBridgeEventHandler] design_get_snapshot: no registered elements in bridge",
              );
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
              // Try finding element by data-testid as last resort
              styleDomEl = document.querySelector<HTMLElement>(`[data-testid="${styleElementId}"]`);
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
              stateDomEl = document.querySelector<HTMLElement>(`[data-testid="${stateElementId}"]`);
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
              // No registered elements available — nothing to audit
              console.log(
                "[UIBridgeEventHandler] design_run_audit: no registered elements in bridge",
              );
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

          // ========== Change Tracking ==========
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
            break;
          }

          // ========== Undo/Redo ==========
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
            break;
          }

          // ========== Forms ==========
          case "get_forms": {
            const { discoverForms } = await import("ui-bridge/ai");
            const formElements = currentBridge.elements
              .filter((el) =>
                ["input", "select", "textarea", "checkbox", "radio"].includes(el.type),
              )
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
            break;
          }

          // ========== Network Requests ==========
          case "get_network_requests":
          case "get_network_requests_in_flight": {
            const { NetworkRequestTracker } = await import("ui-bridge");

            if (!networkTrackerRef.current) {
              networkTrackerRef.current = new NetworkRequestTracker();
              networkTrackerRef.current.install();
            }
            const tracker = networkTrackerRef.current;

            if (type === "get_network_requests_in_flight") {
              const requests = tracker.getInFlight();
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { requests, count: requests.length },
                timestamp: Date.now(),
              });
            } else {
              const params = payload.params ?? {};
              const requests = tracker.getAll(params as Parameters<typeof tracker.getAll>[0]);
              const inFlight = tracker.getInFlight();
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { requests, count: requests.length, inFlightCount: inFlight.length },
                timestamp: Date.now(),
              });
            }
            break;
          }

          // ========== Idle Detection ==========
          case "get_idle_status":
          case "wait_for_idle": {
            const { CompositeIdleDetector } = await import("ui-bridge");

            if (!idleDetectorRef.current) {
              idleDetectorRef.current = CompositeIdleDetector.create();
            }
            const detector = idleDetectorRef.current;

            if (type === "get_idle_status") {
              const status = detector.getStatus();
              await sendResponse({
                requestId,
                type,
                success: true,
                data: status,
                timestamp: Date.now(),
              });
            } else {
              const timeout = (payload.params?.timeout as number) ?? 30000;
              const minStableMs = (payload.params?.minStableMs as number) ?? 500;
              const exclude = (payload.params?.exclude as string[]) ?? undefined;
              const status = await detector.waitForIdle({ timeout, minStableMs, exclude });
              await sendResponse({
                requestId,
                type,
                success: true,
                data: status,
                timestamp: Date.now(),
              });
            }
            break;
          }

          // ========== Stuck Screen Diagnosis ==========
          case "diagnose_stuck_screen": {
            const { CompositeIdleDetector, StuckScreenDetector } = await import("ui-bridge");

            if (!idleDetectorRef.current) {
              idleDetectorRef.current = CompositeIdleDetector.create();
            }

            const stuckDetector = new StuckScreenDetector(idleDetectorRef.current, {
              observationWindowMs: (payload.params?.observationWindowMs as number) ?? 3000,
              domMutationThreshold: (payload.params?.domMutationThreshold as number) ?? 3,
            });

            const diagnosis = await stuckDetector.diagnose();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: diagnosis,
              timestamp: Date.now(),
            });
            break;
          }

          // ========== Keyboard Shortcuts ==========
          case "get_keyboard_shortcuts": {
            const uiBridgeGlobal = getUIBridgeGlobal();
            const tracker = uiBridgeGlobal?.shortcutTracker as
              | { getSnapshotShortcutContext: () => unknown }
              | undefined;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: tracker?.getSnapshotShortcutContext() ?? {
                shortcuts: [],
                totalCount: 0,
                lastScanTimestamp: 0,
              },
              timestamp: Date.now(),
            });
            break;
          }

          // ========== AI Search & Find ==========
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
            break;
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
            break;
          }

          // ========== Find, Workflows, Element State ==========
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
            break;
          }

          case "get_workflows": {
            // Return workflows from snapshot
            const snapshot = await currentBridge.createSnapshotAsync();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                workflows: (snapshot as unknown as Record<string, unknown>).workflows ?? [],
                timestamp: Date.now(),
              },
              timestamp: Date.now(),
            });
            break;
          }

          case "run_workflow": {
            // Execute a workflow via the runner's unified workflow engine
            const workflowId = (payload as unknown as Record<string, unknown>).id as string;
            const runRequest = ((payload as unknown as Record<string, unknown>).request ||
              {}) as Record<string, unknown>;
            try {
              const port = getApiPort();
              const runResponse = await fetch(
                `http://127.0.0.1:${port}/unified-workflows/${encodeURIComponent(workflowId)}/run`,
                {
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify({
                    force_fresh_start:
                      (runRequest.params as Record<string, unknown>)?.force_fresh_start ?? false,
                  }),
                },
              );
              const runResult = (await runResponse.json()) as Record<string, unknown>;
              const runResultData = (runResult.data || {}) as Record<string, unknown>;
              await sendResponse({
                requestId,
                type,
                success: (runResult.success as boolean) ?? runResponse.ok,
                data: {
                  workflowId,
                  runId:
                    runResultData.task_run_id || runResultData.execution_id || `run-${Date.now()}`,
                  status: "running",
                  steps: [],
                  totalSteps: 0,
                  startedAt: Date.now(),
                },
                timestamp: Date.now(),
              });
            } catch (runErr) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: String(runErr),
                timestamp: Date.now(),
              });
            }
            break;
          }

          case "get_workflow_status": {
            // Get workflow run status from the runner
            const statusRunId = (payload as unknown as Record<string, unknown>).runId as string;
            try {
              const port = getApiPort();
              const statusResponse = await fetch(
                `http://127.0.0.1:${port}/task-runs/${encodeURIComponent(statusRunId)}`,
              );
              if (!statusResponse.ok) {
                const errBody = (await statusResponse.json().catch(() => ({}))) as Record<
                  string,
                  unknown
                >;
                await sendResponse({
                  requestId,
                  type,
                  success: false,
                  error: (errBody.error as string) || `Runner returned ${statusResponse.status}`,
                  timestamp: Date.now(),
                });
                break;
              }
              const statusResult = (await statusResponse.json()) as Record<string, unknown>;
              const taskRun = (statusResult.data || statusResult) as Record<string, unknown>;
              await sendResponse({
                requestId,
                type,
                success: true,
                data: {
                  workflowId: taskRun.workflow_id || "",
                  runId: statusRunId,
                  status: mapTaskRunStatus(taskRun.status as string),
                  steps: [],
                  totalSteps: (taskRun.total_steps as number) || 0,
                  currentStep: taskRun.current_step,
                  startedAt: taskRun.created_at
                    ? new Date(taskRun.created_at as string).getTime()
                    : Date.now(),
                  completedAt: taskRun.completed_at
                    ? new Date(taskRun.completed_at as string).getTime()
                    : undefined,
                },
                timestamp: Date.now(),
              });
            } catch (statusErr) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: String(statusErr),
                timestamp: Date.now(),
              });
            }
            break;
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
              return;
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
              return;
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: stateEl.getState(),
              timestamp: Date.now(),
            });
            break;
          }

          // ========== Media Discovery ==========
          case "find_media": {
            const mediaParams = payload.params ?? payload.body ?? {};
            const discovered = await currentBridge.discover({
              ...(mediaParams as Record<string, unknown>),
              mediaOnly: true,
              includeMedia: true,
              includeHidden: true,
            });
            await sendResponse({
              requestId,
              type,
              success: true,
              data: discovered,
              timestamp: Date.now(),
            });
            break;
          }

          case "media_audit": {
            const auditType = (payload.params ?? payload.body ?? ({} as Record<string, unknown>))
              ?.auditType;
            const discovered = await currentBridge.discover({
              mediaOnly: true,
              includeMedia: true,
              includeHidden: true,
            });
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { auditType, elements: discovered },
              timestamp: Date.now(),
            });
            break;
          }

          case "capture_media_snapshot":
          case "analyze_media": {
            // These require browser-side canvas operations — relay to SDK
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { relayRequired: true, command: type, payload },
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

        // Listen for ping and respond with pong (Tauri event + HTTP fallback)
        const unlistenPing = await listen("ui-bridge-ping", async () => {
          try {
            await emit("ui-bridge-pong", { timestamp: Date.now() });
          } catch {
            // Tauri event failed — use HTTP fallback
            await httpSendPong();
          }
        });

        // Also set up a periodic HTTP pong as a safety net in case
        // Tauri events from JS→Rust stop working (WebView2 IPC issue)
        const pongInterval = setInterval(() => {
          httpSendPong().catch(() => {});
        }, 3000);

        console.log("[UIBridgeEventHandler] Listener set up successfully");

        // Store ping unlisten for cleanup
        const originalUnlisten = unlisten;
        unlisten = () => {
          originalUnlisten?.();
          unlistenPing();
          clearInterval(pongInterval);
        };
      } catch (error) {
        console.error("[UIBridgeEventHandler] Failed to set up listener:", error);
      }
    };

    // Log page unload for diagnostics (no pending-request tracking to clean up)
    const handleBeforeUnload = () => {
      console.log("[UIBridgeEventHandler] Page unloading, cleaning up");
    };
    window.addEventListener("beforeunload", handleBeforeUnload);

    setupListener();

    return () => {
      console.log("[UIBridgeEventHandler] Cleaning up listener");
      isMounted = false;
      window.removeEventListener("beforeunload", handleBeforeUnload);
      if (unlisten) {
        unlisten();
      }
      if (networkTrackerRef.current) {
        networkTrackerRef.current.destroy();
        networkTrackerRef.current = null;
      }
      if (idleDetectorRef.current) {
        idleDetectorRef.current.destroy();
        idleDetectorRef.current = null;
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
