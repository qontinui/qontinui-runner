import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { createLogger } from "@/lib/logger";
import {
  TAB_LIST,
  ACTIVE_TAB_STORAGE_KEY,
  isValidTabId,
  type MainTabId,
} from "@/components/app/tab-types";
import { instanceStorage } from "@/lib/instance-storage";

const logger = createLogger("UIBridgePageEvents");

/**
 * Handles: page_refresh, page_navigate, page_go_back, page_go_forward, query_selector, page_evaluate
 */
export function usePageEvents(context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">) {
  const { sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      switch (type) {
        case "page_refresh": {
          // Do NOT call window.location.reload() — the runner is a Tauri app and
          // a full page reload resets all React state (auth, execution, terminals),
          // causing the "Checking authentication..." screen to flash repeatedly.
          logger.debug("page_refresh: ignoring (full reload disabled in runner)");
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { success: true, url: window.location.href },
            timestamp: Date.now(),
          });
          return true;
        }

        case "page_navigate": {
          const { url } = payload;
          // `mode` is optional; defaults to "hard" for back-compat with the
          // pre-F1 envelope `{ url }`. F1 adds "soft", which preserves injected
          // window state (fetch patches, `window.__*` globals) and drives the
          // SPA router via `pushState` + a synthetic `popstate` event.
          const rawMode = (payload as { mode?: unknown }).mode;
          const modeStr = typeof rawMode === "string" ? rawMode : undefined;
          if (modeStr !== undefined && modeStr !== "hard" && modeStr !== "soft") {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `invalid mode: "${modeStr}" (expected "hard" or "soft")`,
              timestamp: Date.now(),
            });
            return true;
          }
          const mode: "hard" | "soft" = modeStr ?? "hard";
          if (!url) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "url is required",
              timestamp: Date.now(),
            });
            return true;
          }
          try {
            // Navigate using the app's tab system via a custom event.
            // Direct history.pushState() only updates the URL but does NOT trigger
            // React re-renders — the SPA never switches tabs/pages.
            const previousPath = window.location.pathname;
            if (url.startsWith("/")) {
              if (mode === "soft") {
                // Soft mode: pure client-side navigation. React Router v6
                // listens to `popstate`, so fire a synthetic PopStateEvent
                // after pushState. Also emit ui-bridge:navigate for any
                // non-router listeners that want to observe navigation.
                window.history.pushState(null, "", url);
                try {
                  window.dispatchEvent(new PopStateEvent("popstate"));
                } catch {
                  // Fallback for very old engines: dispatch a generic Event.
                  window.dispatchEvent(new Event("popstate"));
                }
                window.dispatchEvent(
                  new CustomEvent("ui-bridge:navigate", {
                    detail: { url, mode: "soft" },
                  }),
                );
                // Also notify the runner's tab system so its internal state
                // tracks the URL change — soft navigation still wants the
                // active tab to flip.
                const page = url.replace(/^\/+/, "").replace(/\//g, "-") || "gui-automation";
                window.dispatchEvent(
                  new CustomEvent("ui-bridge-navigate", { detail: { page, url } }),
                );
              } else {
                // Hard mode (default / legacy): preserve existing behaviour.
                // Convert URL path to tab ID: strip leading slash, then
                // replace remaining slashes with hyphens so
                // "/settings/world-state-verifier" becomes
                // "settings-world-state-verifier" — matching the PAGE_TO_TAB
                // keys in useAppNavigation.ts.
                const page = url.replace(/^\/+/, "").replace(/\//g, "-") || "gui-automation";
                window.dispatchEvent(
                  new CustomEvent("ui-bridge-navigate", { detail: { page, url } }),
                );
                // Also update the URL bar for consistency
                window.history.pushState({}, "", url);
              }
            } else {
              console.warn(
                `[UIBridge] page_navigate: ignoring absolute URL navigation in runner: ${url}`,
              );
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url, hard: mode === "hard", mode },
              timestamp: Date.now(),
            });

            try {
              fetch(`http://localhost:${getApiPort()}/ui-bridge/control/render-log`, {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                  type: "navigation",
                  timestamp: Date.now(),
                  from: previousPath,
                  to: url,
                  mode,
                }),
              }).catch(() => {});
            } catch {
              // Non-critical
            }
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Navigation failed: ${err instanceof Error ? err.message : String(err)}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "page_go_back": {
          // Wait for popstate to fire so we get the correct URL and trigger tab switch
          const backUrl = await new Promise<string>((resolve) => {
            const onPop = () => {
              window.removeEventListener("popstate", onPop);
              const page = window.location.pathname.replace(/^\/+/, "") || "gui-automation";
              window.dispatchEvent(
                new CustomEvent("ui-bridge-navigate", {
                  detail: { page, url: window.location.pathname },
                }),
              );
              resolve(window.location.href);
            };
            window.addEventListener("popstate", onPop);
            window.history.back();
            // Fallback timeout in case popstate doesn't fire (e.g., no history)
            setTimeout(() => {
              window.removeEventListener("popstate", onPop);
              resolve(window.location.href);
            }, 500);
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { success: true, url: backUrl },
            timestamp: Date.now(),
          });
          return true;
        }

        case "page_go_forward": {
          // Wait for popstate to fire so we get the correct URL and trigger tab switch
          const fwdUrl = await new Promise<string>((resolve) => {
            const onPop = () => {
              window.removeEventListener("popstate", onPop);
              const page = window.location.pathname.replace(/^\/+/, "") || "gui-automation";
              window.dispatchEvent(
                new CustomEvent("ui-bridge-navigate", {
                  detail: { page, url: window.location.pathname },
                }),
              );
              resolve(window.location.href);
            };
            window.addEventListener("popstate", onPop);
            window.history.forward();
            // Fallback timeout in case popstate doesn't fire (e.g., no forward history)
            setTimeout(() => {
              window.removeEventListener("popstate", onPop);
              resolve(window.location.href);
            }, 500);
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { success: true, url: fwdUrl },
            timestamp: Date.now(),
          });
          return true;
        }

        case "scroll_page": {
          const scrollParams = payload.params ?? {};
          const beforeX = window.scrollX;
          const beforeY = window.scrollY;
          const useSmooth = !!(scrollParams.smooth as boolean);
          window.scrollBy({
            top: (scrollParams.top as number) ?? 0,
            left: (scrollParams.left as number) ?? 0,
            behavior: useSmooth ? "smooth" : "auto",
          });

          // When smooth scrolling, the position updates asynchronously.
          // Wait for the scrollend event (with a fallback timeout) before reading.
          if (useSmooth) {
            await new Promise<void>((resolve) => {
              const onScrollEnd = () => {
                window.removeEventListener("scrollend", onScrollEnd);
                clearTimeout(fallback);
                resolve();
              };
              window.addEventListener("scrollend", onScrollEnd, { once: true });
              // Fallback: if scrollend never fires (e.g. no actual scroll or
              // browser doesn't support scrollend), resolve after 500ms
              const fallback = setTimeout(() => {
                window.removeEventListener("scrollend", onScrollEnd);
                resolve();
              }, 500);
            });
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              before: { scrollX: beforeX, scrollY: beforeY },
              after: { scrollX: window.scrollX, scrollY: window.scrollY },
              changed: window.scrollX !== beforeX || window.scrollY !== beforeY,
            },
            timestamp: Date.now(),
          });
          return true;
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
            return true;
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
          return true;
        }

        case "page_evaluate": {
          const { expression, unwrap } = payload;
          if (!expression) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "expression is required",
              timestamp: Date.now(),
            });
            return true;
          }
          try {
            // SECURITY: Block dangerous patterns while allowing diagnostic expressions.
            // Uses new Function() which does not have access to local scope.
            //
            // Note: `window.location` read access (pathname, host, port, href,
            // origin, protocol, search, hash, hostname) is allowed for test
            // probes. Only mutating methods (assign/replace/reload) and
            // assignment to window.location are blocked.
            // Block patterns that enable code injection, data exfiltration,
            // or persistent state corruption. Deliberately ALLOW:
            //   - localStorage / sessionStorage (needed for tab navigation)
            //   - location.reload (needed for page refresh after config changes)
            //   - globalThis / Reflect / Proxy (useful for deep inspection)
            // The remaining blocklist targets patterns with no legitimate
            // agent-testing use case.
            const dangerousPatterns = [
              /\bimport\s*\(/, // dynamic import — can load arbitrary modules
              /\brequire\s*\(/, // require — same
              /\b__proto__\b/, // prototype pollution
              /\bconstructor\s*\[/, // constructor bracket access
              /\beval\s*\(/, // nested eval — full code execution
              /\bnew\s+Function\b/, // Function constructor — same
              /\bfetch\s*\(/, // network requests — data exfiltration
              /\bXMLHttpRequest\b/, // network requests — same
              /\bnavigator\.sendBeacon\b/, // data exfiltration beacon
              /\bWebSocket\b/, // persistent network channel
              /\bdocument\.cookie\b/, // cookie theft
              /\bwindow\.open\b/, // popup/phishing
              /\bwindow\.location\.(assign|replace)\b/, // redirect (reload allowed)
              /\bwindow\.location\s*=/, // redirect via assignment
              /\blocation\.(assign|replace)\b/, // bare redirect (reload allowed)
              /\blocation\s*=\s*["'`]/, // redirect via bare assignment
              /\bcrypto\.subtle\b/, // key material access
            ];
            const isDangerous = dangerousPatterns.some((p) => p.test(expression));
            if (isDangerous) {
              const matched = dangerousPatterns.find((p) => p.test(expression));
              throw new Error(
                `Expression rejected: contains prohibited pattern${matched ? ` (${matched.source})` : ""}`,
              );
            }
            // Default: wrap as `return <expr>` so simple expressions like
            // `document.title` evaluate to their value. If that's a SyntaxError
            // (e.g., the caller passed multiple statements with top-level
            // `let`/`const` and an explicit `return`), re-run as a raw function
            // body so modern JS works without forcing IIFE wrappers on callers.
            let result: unknown;
            try {
              result = new Function("return " + expression)();
            } catch (firstErr) {
              if (firstErr instanceof SyntaxError) {
                result = new Function(expression)();
              } else {
                throw firstErr;
              }
            }
            const resolvedResult = await Promise.resolve(result);
            if (unwrap) {
              // Opt-in consistent shape: always `{ value, type }`.
              let valueType: "scalar" | "object" | "undefined" | "function" | "null";
              let normalizedValue: unknown;
              if (resolvedResult === null) {
                valueType = "null";
                normalizedValue = null;
              } else if (resolvedResult === undefined) {
                valueType = "undefined";
                normalizedValue = undefined;
              } else if (typeof resolvedResult === "function") {
                valueType = "function";
                // Functions aren't structured-clone-safe. Surface the name
                // (or `<anonymous>`) so the caller gets something useful.
                normalizedValue = (resolvedResult as { name?: string }).name || "<anonymous>";
              } else if (typeof resolvedResult === "object") {
                valueType = "object";
                normalizedValue = resolvedResult;
              } else {
                valueType = "scalar";
                normalizedValue = resolvedResult;
              }
              await sendResponse({
                requestId,
                type,
                success: true,
                data: { value: normalizedValue, type: valueType },
                timestamp: Date.now(),
              });
            } else {
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
            }
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        // ============================================================
        // App-agnostic convenience endpoints (IPC fallback)
        // These delegate to the SDK handlers running in the same webview.
        // ============================================================

        case "click_by_text": {
          try {
            const { text, tag, exact } = (payload.params || {}) as {
              text?: string;
              tag?: string;
              exact?: boolean;
            };
            if (!text) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "text is required",
                timestamp: Date.now(),
              });
              return true;
            }
            const selector = tag || "*";
            const searchText = text.toLowerCase();
            const candidates = document.querySelectorAll<HTMLElement>(selector);
            let clicked = false;
            for (const el of candidates) {
              const elText = el.textContent?.trim().toLowerCase() ?? "";
              if (exact ? elText === searchText : elText.includes(searchText)) {
                el.click();
                clicked = true;
                await sendResponse({
                  requestId,
                  type,
                  success: true,
                  data: {
                    clicked: true,
                    element: {
                      tag: el.tagName.toLowerCase(),
                      text: el.textContent?.trim().slice(0, 200),
                    },
                  },
                  timestamp: Date.now(),
                });
                break;
              }
            }
            if (!clicked) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `No element found with text "${text}"`,
                timestamp: Date.now(),
              });
            }
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "click_by_selector": {
          try {
            const { selector, index } = (payload.params || {}) as {
              selector?: string;
              index?: number;
            };
            if (!selector) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "selector is required",
                timestamp: Date.now(),
              });
              return true;
            }
            const el =
              index != null
                ? document.querySelectorAll<HTMLElement>(selector)[index]
                : document.querySelector<HTMLElement>(selector);
            if (!el) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `No element found for selector "${selector}"`,
                timestamp: Date.now(),
              });
              return true;
            }
            el.click();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                clicked: true,
                element: {
                  tag: el.tagName.toLowerCase(),
                  text: el.textContent?.trim().slice(0, 200),
                },
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
          return true;
        }

        case "type_into": {
          try {
            const { selector, label, text, clear } = (payload.params || {}) as {
              selector?: string;
              label?: string;
              text?: string;
              clear?: boolean;
            };
            if (!text) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "text is required",
                timestamp: Date.now(),
              });
              return true;
            }
            if (!label && !selector) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "Either label or selector is required",
                timestamp: Date.now(),
              });
              return true;
            }
            let el: HTMLElement | null = null;
            if (label) {
              const labels = document.querySelectorAll<HTMLLabelElement>("label");
              for (const lbl of labels) {
                if (lbl.textContent?.trim().toLowerCase().includes(label.toLowerCase())) {
                  el = lbl.htmlFor
                    ? document.getElementById(lbl.htmlFor)
                    : lbl.querySelector("input, select, textarea");
                  if (el) break;
                }
              }
            } else if (selector) {
              el = document.querySelector<HTMLElement>(selector);
            }
            if (!el) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `No input found`,
                timestamp: Date.now(),
              });
              return true;
            }
            el.focus();
            if (clear && "value" in el) (el as HTMLInputElement).value = "";
            if ("value" in el) {
              (el as HTMLInputElement).value += text;
              el.dispatchEvent(new Event("input", { bubbles: true }));
              el.dispatchEvent(new Event("change", { bubbles: true }));
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { typed: true },
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
          return true;
        }

        case "read_value": {
          try {
            const { selector } = (payload.params || {}) as { selector?: string };
            if (!selector) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "selector is required",
                timestamp: Date.now(),
              });
              return true;
            }
            const el = document.querySelector<HTMLElement>(selector);
            if (!el) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `No element found for "${selector}"`,
                timestamp: Date.now(),
              });
              return true;
            }
            const value = "value" in el ? (el as HTMLInputElement).value : (el.textContent ?? null);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { value, length: value?.length ?? 0 },
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
          return true;
        }

        case "find_by_text": {
          try {
            const { text, tag, exact } = (payload.params || {}) as {
              text?: string;
              tag?: string;
              exact?: boolean;
            };
            if (!text) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "text is required",
                timestamp: Date.now(),
              });
              return true;
            }
            const candidates = document.querySelectorAll<HTMLElement>(tag || "*");
            const searchText = text.toLowerCase();
            const results: Array<{ index: number; tag: string; text: string; visible: boolean }> =
              [];
            let idx = 0;
            for (const el of candidates) {
              const elText = el.textContent?.trim().toLowerCase() ?? "";
              if (exact ? elText === searchText : elText.includes(searchText)) {
                results.push({
                  index: idx++,
                  tag: el.tagName.toLowerCase(),
                  text: el.textContent?.trim().slice(0, 200) ?? "",
                  visible: el.offsetParent !== null,
                });
              }
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: results,
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
          return true;
        }

        case "get_diagnostics": {
          try {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const globalBridge = (window as any).__UI_BRIDGE__;
            const registeredCount = globalBridge?.registry?.getAllElements?.()?.length ?? 0;
            const interactiveSelectors =
              "a[href],button,input,select,textarea,[role='button'],[role='link'],[role='checkbox'],[role='radio'],[role='menuitem'],[role='tab'],[role='switch'],[role='slider'],[tabindex]:not([tabindex='-1']),[contenteditable='true'],[onclick]";
            const domCount = document.querySelectorAll(interactiveSelectors).length;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                sdk_initialized: !!globalBridge,
                auto_register_active: !!globalBridge?.autoRegisterActive,
                registered_elements: registeredCount,
                dom_interactive_elements: domCount,
                mutation_observer_active: !!globalBridge?.mutationObserverActive,
                navigation_adapter: "window-location",
                page_title: document.title,
                page_url: window.location.href,
                page_ready: document.readyState === "complete",
                providers_mounted: globalBridge?.providers ?? [],
                last_discover_at: null,
                capabilities: ["control", "find", "ai"],
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
          return true;
        }

        case "get_routes": {
          await sendResponse({
            requestId,
            type,
            success: true,
            data: [{ name: document.title || "Current Page", path: window.location.pathname }],
            timestamp: Date.now(),
          });
          return true;
        }

        case "navigate_by_adapter": {
          try {
            const { page } = (payload.params || {}) as { page?: string };
            if (!page) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "page is required",
                timestamp: Date.now(),
              });
              return true;
            }
            const target = page.startsWith("/") || page.startsWith("http") ? page : "/" + page;
            // Navigate using the app's tab system via CustomEvent (same pattern
            // as page_navigate). Direct pushState alone does NOT trigger React
            // re-renders — the SPA never switches tabs/pages.
            if (target.startsWith("http") && !target.startsWith(window.location.origin)) {
              // External URL — full navigation required
              window.location.href = target;
            } else {
              const path = target.startsWith("http") ? new URL(target).pathname : target;
              const tabName = path.replace(/^\/+/, "") || "gui-automation";
              window.dispatchEvent(
                new CustomEvent("ui-bridge-navigate", { detail: { page: tabName, url: path } }),
              );
              window.history.pushState({}, "", path);
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { navigated: true, route: { name: document.title, path: page } },
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
          return true;
        }

        // ============================================================
        // F4 — First-class tab activation
        // ============================================================

        case "tabs_list": {
          try {
            const activeTab = instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY) ?? "prompt-home";
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                activeTab,
                tabs: TAB_LIST.map((entry) => ({ id: entry.id, label: entry.label })),
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
          return true;
        }

        case "tab_activate": {
          try {
            const tabId =
              (payload as { tabId?: unknown }).tabId ??
              (payload.params as { tabId?: unknown } | undefined)?.tabId;
            if (typeof tabId !== "string" || tabId.length === 0) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "tabId is required (non-empty string)",
                data: {
                  error: "missing_tab_id",
                  knownTabs: TAB_LIST.map((t) => t.id),
                },
                timestamp: Date.now(),
              });
              return true;
            }
            if (!isValidTabId(tabId)) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `unknown tabId: "${tabId}"`,
                data: {
                  error: "unknown_tab",
                  knownTabs: TAB_LIST.map((t) => t.id),
                },
                timestamp: Date.now(),
              });
              return true;
            }
            const previousTab = instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY) ?? "prompt-home";
            // Fire the same event the runner's `useAppNavigation` hook already
            // listens for. This is the exact code path a user click traverses
            // (via the sidebar's `setActiveTab(tab)` -> `ui-bridge-set-tab`),
            // so lazy-mounts, `url-state-sync`, the `activeTab`-keyed effects
            // in `useAppNavigation`, etc. all fire as usual.
            window.dispatchEvent(
              new CustomEvent<{ tab: MainTabId }>("ui-bridge-set-tab", {
                detail: { tab: tabId as MainTabId },
              }),
            );
            // Persist immediately so that `tabs_list` polled right after sees
            // the new value (the React effect in `useAppNavigation` would also
            // persist on the next render, but agents may read back before that).
            instanceStorage.setItem(ACTIVE_TAB_STORAGE_KEY, tabId);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                activeTab: tabId,
                previousTab,
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
          return true;
        }

        case "register_network_stub": {
          // F2 — install a fetch stub. Lazily installs the SDK's stub-fetch
          // interceptor on first call so the runner can short-circuit
          // matching requests without also enabling the full network-request
          // tracker (which is installed separately via NetworkRequestTracker).
          try {
            const { getGlobalStubRegistry, installStubFetchInterceptor, validateStubRequest } =
              await import("@qontinui/ui-bridge");
            const spec =
              (payload.params as unknown as Record<string, unknown>) ??
              (payload as unknown as Record<string, unknown>);
            const err = validateStubRequest(spec);
            if (err) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: err.message,
                data: { error: "invalid_stub", field: err.field },
                timestamp: Date.now(),
              });
              return true;
            }
            installStubFetchInterceptor();
            const registry = getGlobalStubRegistry();
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const id = registry.register(spec as any);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { id },
              timestamp: Date.now(),
            });
          } catch (e) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(e),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "list_network_stubs": {
          try {
            const { getGlobalStubRegistry } = await import("@qontinui/ui-bridge");
            const registry = getGlobalStubRegistry();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { stubs: registry.list() },
              timestamp: Date.now(),
            });
          } catch (e) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(e),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "delete_network_stub": {
          try {
            const { getGlobalStubRegistry } = await import("@qontinui/ui-bridge");
            const id =
              (payload as { id?: unknown }).id ??
              (payload.params as { id?: unknown } | undefined)?.id;
            if (typeof id !== "string" || id.length === 0) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: "id is required (non-empty string)",
                timestamp: Date.now(),
              });
              return true;
            }
            const removed = getGlobalStubRegistry().delete(id);
            if (!removed) {
              await sendResponse({
                requestId,
                type,
                success: false,
                error: `stub not found: ${id}`,
                data: { error: "not_found", id },
                timestamp: Date.now(),
              });
              return true;
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { id, deleted: true },
              timestamp: Date.now(),
            });
          } catch (e) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(e),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "clear_network_stubs": {
          try {
            const { getGlobalStubRegistry } = await import("@qontinui/ui-bridge");
            const cleared = getGlobalStubRegistry().clear();
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { cleared },
              timestamp: Date.now(),
            });
          } catch (e) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(e),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        default:
          return false;
      }
    },
    [sendResponse],
  );
}
