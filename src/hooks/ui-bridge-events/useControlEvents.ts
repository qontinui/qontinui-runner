import { useCallback } from "react";
import type { ControlActionRequest, StableRefResolution } from "@qontinui/ui-bridge";
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
 * Normalize the IPC `action` field into the SDK request that is FORWARDED
 * WHOLE to `executeAction`.
 *
 * The envelope is returned by identity, never rebuilt. That is the entire
 * point: this call site used to construct `{action, params, waitOptions}` from
 * a hand-written three-field payload type, and every per-request opt-in the
 * Rust layer threaded into the IPC payload — `verifyEffect` (D3 effect
 * calculus), `fromSnapshotId` (the pre-action staleness gate),
 * `includeResolutionAlternates` (the ranked selector alternates) — was
 * silently dropped on the floor here. The SDK read `request.<field>`, saw
 * `undefined`, and the opt-in was unreachable on every runner transport no
 * matter how carefully `sdk_client.rs` and `elements.rs` forwarded it.
 *
 * A field the SDK adds to `ControlActionRequest` therefore needs no change
 * here at all — which is the property a per-field roster could never have.
 *
 * `string` is the SDK proxy-fallback spelling (a bare verb, no envelope); it
 * is the ONE input that has to be constructed, because there is no envelope to
 * forward.
 */
export function toActionRequest(
  action: NonNullable<UIBridgeRequestPayload["action"]>,
): ControlActionRequest {
  return typeof action === "string" ? { action } : action;
}

/**
 * Project a `resolveStableRef` result onto the `resolve_stable_ref` IPC
 * response body.
 *
 * `resolveStableRef` returns `StableRefResolution { element, resolution }` —
 * the live `RegisteredElement` plus WHICH of the four strategies produced it.
 * It used to return the bare `RegisteredElement`, and this projection still
 * read `resolved.id` / `resolved.mounted`: both `undefined` on the new shape,
 * so a SUCCESSFUL resolution answered `{elementId: undefined}` and was
 * indistinguishable from a miss to the stable-ref retry in `elements.rs`,
 * which reads `elementId` as a string.
 *
 * `resolution` is passed through as well, so the retry path has the strategy
 * and stability class behind the id it is about to act on.
 */
export function stableRefResponseData(resolved: StableRefResolution | null): {
  elementId: string | null;
  mounted?: boolean;
  resolution?: StableRefResolution["resolution"];
} {
  if (!resolved) return { elementId: null };
  return {
    elementId: resolved.element.id,
    mounted: resolved.element.mounted,
    resolution: resolved.resolution,
  };
}

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

          // Normalize: `action` may be a bare verb (the SDK proxy fallback)
          // or the full request envelope (the control endpoint). Everything
          // past this point treats it as the SDK's own `ControlActionRequest`.
          const actionObj = toActionRequest(action);

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

          // Forwarded WHOLE — see `toActionRequest` for why this must never
          // become a field-by-field rebuild again.
          const result = await currentBridge.executeAction(elementId, actionObj);

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
              data: stableRefResponseData(resolved),
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

        case "dispatch_key": {
          // Document/window-level key dispatch — backs BOTH
          // `POST /ui-bridge/control/key` and the SDK-declared
          // `POST /ui-bridge/control/page/send-keys`
          // (src-tauri/src/mcp/ui_bridge/keyboard.rs). The two routes differ
          // only in their request grammar and default `target`; the Rust layer
          // normalizes both before they reach here.
          //
          // This lives here (and not in the element-scoped `execute_action`
          // path) because the runner's global shortcut listeners are attached
          // to `window`, not to any registered element — e.g. Ctrl+Shift+B,
          // which toggles `workflowGen.showSidebar` and is the only way to
          // render the session-manager sidebar / Worktrees panel. The
          // per-element action gate correctly refuses `keyboard` on elements
          // that don't advertise it, so automation had no way in before this.
          //
          // ⚠ `target: "activeElement"` is the ONE target that can type into a
          // focused field. On a runner that field is frequently a terminal
          // bound to a LIVE Claude/PowerShell session, so an unintended
          // dispatch injects text into someone's real work. It is opt-in only
          // on both routes: `/control/key` defaults `target` to `"window"` and
          // `/control/page/send-keys` to `"document"` (the SDK contract's
          // default) — neither can land text in a focused field.
          const params = (payload.params ?? {}) as {
            keys?: Array<{
              key?: string;
              modifiers?: {
                ctrl?: boolean;
                shift?: boolean;
                alt?: boolean;
                meta?: boolean;
              };
            }>;
            target?: string;
            /** Milliseconds between keys — SDK `sendKeysToPage` contract. */
            delay?: number;
          };
          const keys = Array.isArray(params.keys) ? params.keys : [];
          if (keys.length === 0) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "keys is required and must be a non-empty array",
              timestamp: Date.now(),
            });
            return true;
          }

          const targetName = params.target || "window";
          let target: EventTarget | null;
          switch (targetName) {
            case "window":
              target = window;
              break;
            case "document":
              target = document;
              break;
            case "body":
              target = document.body;
              break;
            case "activeElement":
              target = document.activeElement ?? document.body;
              break;
            default:
              target = null;
          }
          if (!target) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Unknown dispatch target '${targetName}' — expected window, document, body, or activeElement`,
              timestamp: Date.now(),
            });
            return true;
          }

          // Only printable characters (single char, no ctrl/alt/meta) produce a
          // keypress event — same rule as the SDK's `sendKeys` handler
          // (ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts).
          const shouldKeypress = (
            key: string,
            mods: { ctrl?: boolean; alt?: boolean; meta?: boolean },
          ) => key.length === 1 && !mods.ctrl && !mods.alt && !mods.meta;

          // `KeyboardEvent.code` is the PHYSICAL key, and it is how a
          // layout-independent shortcut is bound. Leaving it unset (as this
          // loop did) ships every synthetic event with `code: ""`, so any
          // listener that reads `e.code` can never match one — a dispatch that
          // reports success while reaching nothing. Mirrors the SDK's
          // `keyToCode` (ui-bridge `core/key-events.ts`), which both the
          // element-scoped `sendKeys` action and `sendKeysToPage` use, so the
          // event shape is the same whichever side executes the dispatch.
          const keyToCode = (key: string): string => {
            if (key.length === 1) {
              const upper = key.toUpperCase();
              if (upper >= "A" && upper <= "Z") return `Key${upper}`;
              if (upper >= "0" && upper <= "9") return `Digit${upper}`;
              if (key === " ") return "Space";
            }
            return key;
          };

          // `keyCode` / `which` are the LEGACY numeric key identifiers. They
          // are deprecated in the spec and still read by a large amount of real
          // handler code — xterm.js, CodeMirror, every `e.keyCode === 13`
          // Enter check, and the jQuery-era `which` idiom. Omitting them (as
          // this loop did) ships every synthetic event with `keyCode: 0`, so
          // such a listener sees a keystroke it cannot identify: the same
          // "dispatch reports success while reaching nothing" failure the
          // `code` fix above closed, one field over.
          //
          // Mirrors the same table `keyToCode` mirrors. When ui-bridge's
          // `core/key-events.ts` builder ships in a published
          // `@qontinui/ui-bridge` — it names this copy as "the third duplicate
          // this module exists to retire" — delete both local tables and call
          // it instead; the runner consumes the SDK as a package, so it cannot
          // import an unpublished one today.
          const NAMED_KEY_CODES: Record<string, number> = {
            Backspace: 8,
            Tab: 9,
            Enter: 13,
            Shift: 16,
            Control: 17,
            Alt: 18,
            Pause: 19,
            CapsLock: 20,
            Escape: 27,
            PageUp: 33,
            PageDown: 34,
            End: 35,
            Home: 36,
            ArrowLeft: 37,
            ArrowUp: 38,
            ArrowRight: 39,
            ArrowDown: 40,
            Insert: 45,
            Delete: 46,
            Meta: 91,
            ContextMenu: 93,
            NumLock: 144,
            ScrollLock: 145,
            F1: 112,
            F2: 113,
            F3: 114,
            F4: 115,
            F5: 116,
            F6: 117,
            F7: 118,
            F8: 119,
            F9: 120,
            F10: 121,
            F11: 122,
            F12: 123,
          };
          // Punctuation keyCodes are the US-layout PHYSICAL key numbers, which
          // is what a `keyCode` has always meant — `;` and `:` are both 186.
          const PUNCTUATION_KEY_CODES: Record<string, number> = {
            ";": 186,
            ":": 186,
            "=": 187,
            "+": 187,
            ",": 188,
            "<": 188,
            "-": 189,
            _: 189,
            ".": 190,
            ">": 190,
            "/": 191,
            "?": 191,
            "`": 192,
            "~": 192,
            "[": 219,
            "{": 219,
            "\\": 220,
            "|": 220,
            "]": 221,
            "}": 221,
            "'": 222,
            '"': 222,
          };
          const keyToKeyCode = (key: string): number => {
            if (key.length === 1) {
              const upper = key.toUpperCase();
              if (upper >= "A" && upper <= "Z") return upper.charCodeAt(0);
              if (key >= "0" && key <= "9") return key.charCodeAt(0);
              if (key === " ") return 32;
              const punctuation = PUNCTUATION_KEY_CODES[key];
              if (punctuation !== undefined) return punctuation;
              return upper.charCodeAt(0);
            }
            return NAMED_KEY_CODES[key] ?? 0;
          };

          const delay = typeof params.delay === "number" ? params.delay : 0;
          let dispatched = 0;
          let defaultPrevented = false;
          const dispatchedKeys: string[] = [];
          // Per-key outcome — the SDK's `sendKeysToPage` contract
          // (ui-bridge core/key-events.ts `KeyDispatchOutcome`). A single
          // last-keydown flag cannot say WHICH key a listener consumed.
          const outcomes: Array<{ key: string; defaultPrevented: boolean }> = [];
          for (const keyDesc of keys) {
            const key = keyDesc?.key;
            if (!key) continue;
            const mods = keyDesc.modifiers ?? {};
            const legacyKeyCode = keyToKeyCode(key);
            const eventInit: KeyboardEventInit = {
              key,
              code: keyToCode(key),
              keyCode: legacyKeyCode,
              which: legacyKeyCode,
              bubbles: true,
              cancelable: true,
              ctrlKey: !!mods.ctrl,
              shiftKey: !!mods.shift,
              altKey: !!mods.alt,
              metaKey: !!mods.meta,
            };
            const keydown = new KeyboardEvent("keydown", eventInit);
            target.dispatchEvent(keydown);
            // `dispatchEvent` returns false when a listener called
            // preventDefault() — i.e. a handler actually consumed the shortcut.
            defaultPrevented = keydown.defaultPrevented;
            outcomes.push({ key, defaultPrevented: keydown.defaultPrevented });
            dispatchedKeys.push(key);
            if (shouldKeypress(key, mods)) {
              target.dispatchEvent(new KeyboardEvent("keypress", eventInit));
            }
            target.dispatchEvent(new KeyboardEvent("keyup", eventInit));
            dispatched += 1;
            if (delay > 0) {
              await new Promise<void>((resolve) => setTimeout(resolve, delay));
            }
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            // `defaultPrevented` (last keydown) is kept alongside the new
            // fields so existing `/control/key` callers are unaffected.
            data: {
              dispatched,
              target: targetName,
              defaultPrevented,
              keys: dispatchedKeys,
              outcomes,
            },
            timestamp: Date.now(),
          });
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
