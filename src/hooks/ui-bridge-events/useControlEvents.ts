import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import type { MainTabId } from "@/components/app/tab-types";
import { migrateTabId } from "@/components/app/tab-types";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import {
  closestElementIds,
  serializeElement,
  serializeComponent,
  isElementActionAllowed,
} from "./utils";

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
          // `currentRouteOnly` filtering lives in the Rust
          // `control/snapshot` handler, not here. On the runner it is a
          // no-op because every tab renders under the same pathname — see
          // `get_routes` in usePageEvents.ts for the full rationale. We
          // surface a small `filterInfo` hint alongside the elements so
          // snapshot consumers on the runner can detect the no-op without
          // having to know the Tauri route semantics.
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

          // Element-not-found 404: enrich the IPC error payload with up
          // to 5 closest-match ids by Levenshtein distance so callers can
          // recover from typos without a manual snapshot dance. The
          // candidate set unions registry ids with whatever discover/find
          // surfaced (so freshly-discovered elements are still suggestable
          // even when the caller is asking by a slightly-wrong id).
          const knownIds = new Set<string>(currentBridge.elements.map((e) => e.id));
          try {
            const discovered = await currentBridge.discover({ includeHidden: true });
            for (const e of discovered.elements) knownIds.add(e.id);
          } catch {
            // Discovery for hint generation is best-effort — fall through
            // with whatever the registry already had.
          }
          const closestMatches = closestElementIds(elementId, Array.from(knownIds));
          await sendResponse({
            requestId,
            type,
            success: false,
            error: `Element not found: ${elementId}`,
            hint: closestMatches.length > 0 ? { closestMatches } : undefined,
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

          // Action-not-allowed pre-check: when the element is registered and
          // we already know its declared action set, surface a hint with the
          // allowed actions before dispatching. The SDK's executeAction
          // already validates against a global supported set, but per-element
          // gating ("submit on a div") happens silently — agents need to know
          // which actions the registry actually advertises for this element.
          const targetElement = currentBridge.getElement(elementId);
          if (targetElement) {
            const builtinActions = Array.isArray(targetElement.actions)
              ? targetElement.actions
              : [];
            const customActions = targetElement.customActions
              ? Object.keys(targetElement.customActions)
              : [];
            const allowedActions = [...builtinActions, ...customActions];
            // `isElementActionAllowed` exempts `hoverClick` (a click-variant)
            // wherever `click` is advertised, mirroring the runner-side Rust
            // `is_action_advertised` gate so the two layers can't disagree.
            if (!isElementActionAllowed(allowedActions, actionObj.action)) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `Action '${actionObj.action}' is not allowed for element '${elementId}'`,
                hint: { allowedActions },
                timestamp: Date.now(),
              });
              return true;
            }
          }

          const result = await currentBridge.executeAction(elementId, {
            action: actionObj.action,
            params: actionObj.params,
            waitOptions: actionObj.waitOptions,
          });

          // If the action failed because the element wasn't found at all,
          // enrich the response with closest-match element-id suggestions
          // so callers can recover from typos in the same round-trip the
          // get_element 404 path would have produced.
          let actionHint: { closestMatches?: string[] } | undefined;
          if (
            result.success === false &&
            typeof result.error === "string" &&
            /element not found/i.test(result.error)
          ) {
            const knownIds = new Set<string>(currentBridge.elements.map((e) => e.id));
            try {
              const discovered = await currentBridge.discover({ includeHidden: true });
              for (const e of discovered.elements) knownIds.add(e.id);
            } catch {
              // Best-effort — keep whatever the registry already had.
            }
            const closestMatches = closestElementIds(elementId, Array.from(knownIds));
            if (closestMatches.length > 0) {
              actionHint = { closestMatches };
            }
          }

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
            hint: actionHint,
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
              // Phase 5: emit the canonical Wave-1 diagnostic code so the
              // Rust consumer discriminates on the enum mirror instead of
              // string-matching this prose. Prose `error` is retained as a
              // dual-audience feature (plan goal #3), not for BC.
              error: `Component not found: ${componentId}`,
              code: "UB-ELEM-NOT-FOUND",
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

        case "assert_element": {
          const assertPayload = payload as unknown as Record<string, unknown>;
          const assertElementId = (assertPayload.elementId ?? assertPayload.id) as string;
          const spec = (assertPayload.spec ?? {}) as Record<string, unknown>;
          if (!assertElementId) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                passed: false,
                checked: 0,
                passedCount: 0,
                failures: [],
                error: "ELEMENT_NOT_FOUND",
                // Phase 5: canonical Wave-1 diagnostic code for enum-based
                // discrimination Rust-side. Prose retained (dual-audience).
                errorCode: "UB-ELEM-NOT-FOUND",
                errorMessage: "elementId is required",
              },
              timestamp: Date.now(),
            });
            return true;
          }

          const reg = currentBridge.elements.find((e) => e.id === assertElementId);
          if (!reg) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                passed: false,
                checked: 0,
                passedCount: 0,
                failures: [],
                error: "ELEMENT_NOT_FOUND",
                // Phase 5: canonical Wave-1 diagnostic code for enum-based
                // discrimination Rust-side. Prose retained (dual-audience).
                errorCode: "UB-ELEM-NOT-FOUND",
                errorMessage: `Element '${assertElementId}' not found`,
              },
              timestamp: Date.now(),
            });
            return true;
          }

          const elState = (reg.getState?.() ?? {}) as unknown as Record<string, unknown>;
          const htmlEl = reg.element as HTMLElement | null;
          const failures: Array<{
            field: string;
            expected: unknown;
            actual: unknown;
            kind: string;
          }> = [];
          let checked = 0;

          const es = elState as Record<string, unknown>;
          if (spec.visible !== undefined) {
            checked++;
            if (es.visible !== spec.visible)
              failures.push({
                field: "visible",
                expected: spec.visible,
                actual: es.visible,
                kind: "exact",
              });
          }
          if (spec.enabled !== undefined) {
            checked++;
            if (es.enabled !== spec.enabled)
              failures.push({
                field: "enabled",
                expected: spec.enabled,
                actual: es.enabled,
                kind: "exact",
              });
          }
          if (spec.text !== undefined) {
            checked++;
            const t = (es.text ?? htmlEl?.textContent ?? "") as string;
            if (t !== spec.text)
              failures.push({ field: "text", expected: spec.text, actual: t, kind: "exact" });
          }
          if (spec.textContains !== undefined) {
            checked++;
            const t = (es.text ?? htmlEl?.textContent ?? "") as string;
            if (!t.includes(spec.textContains as string))
              failures.push({
                field: "textContains",
                expected: spec.textContains,
                actual: t,
                kind: "contains",
              });
          }
          if (spec.value !== undefined) {
            checked++;
            if (es.value !== spec.value)
              failures.push({
                field: "value",
                expected: spec.value,
                actual: es.value,
                kind: "exact",
              });
          }
          if (spec.checked !== undefined) {
            checked++;
            if (es.checked !== spec.checked)
              failures.push({
                field: "checked",
                expected: spec.checked,
                actual: es.checked,
                kind: "exact",
              });
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              passed: failures.length === 0,
              checked,
              passedCount: checked - failures.length,
              failures,
              elementSnapshot: {
                id: reg.id,
                type: reg.type,
                text: es.text,
                visible: es.visible,
                enabled: es.enabled,
              },
            },
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
            const { resolveStableRef } = await import("@qontinui/ui-bridge/core");
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

        case "receive_heartbeat": {
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { received: true },
            timestamp: Date.now(),
          });
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
