import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { createLogger } from "@/lib/logger";

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
              // Strip leading slash to get the page/tab name
              const page = url.replace(/^\/+/, "") || "gui-automation";
              window.dispatchEvent(
                new CustomEvent("ui-bridge-navigate", { detail: { page, url } }),
              );
              // Also update the URL bar for consistency
              window.history.pushState({}, "", url);
            } else {
              console.warn(
                `[UIBridge] page_navigate: ignoring absolute URL navigation in runner: ${url}`,
              );
            }
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { success: true, url },
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
          const { expression } = payload;
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
            const dangerousPatterns = [
              /\bimport\s*\(/, // dynamic import
              /\brequire\s*\(/, // require
              /\b__proto__\b/, // prototype pollution
              /\bconstructor\s*\[/, // constructor bracket access
              /\beval\s*\(/, // nested eval
              /\bnew\s+Function\b/, // Function constructor
              /\bfetch\s*\(/, // network requests
              /\bXMLHttpRequest\b/, // network requests
              /\bnavigator\.sendBeacon\b/, // data exfiltration
              /\blocalStorage\b/, // storage access
              /\bsessionStorage\b/, // storage access
              /\bindexedDB\b/, // storage access
              /\bglobalThis\b/, // global scope access
              /\bReflect\b/, // metaprogramming
              /\bProxy\b/, // metaprogramming
              /\bWebSocket\b/, // network access
              /\bwindow\.open\b/, // popup/navigation
              /\bwindow\.location\b/, // navigation/redirect
              /\bdocument\.cookie\b/, // cookie access
              /\bWorker\b/, // web workers
              /\bSharedWorker\b/, // shared workers
              /\bServiceWorker\b/, // service workers
              /\bcrypto\.subtle\b/, // cryptographic operations
            ];
            const isDangerous = dangerousPatterns.some((p) => p.test(expression));
            if (isDangerous) {
              throw new Error("Expression rejected: contains prohibited pattern");
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
          return true;
        }

        default:
          return false;
      }
    },
    [sendResponse],
  );
}
