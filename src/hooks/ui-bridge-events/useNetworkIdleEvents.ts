import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

/**
 * Handles: get_network_requests, get_network_requests_in_flight,
 *          get_idle_status, wait_for_idle, wait_for_navigation_complete,
 *          wait_for_element_stable, diagnose_stuck_screen, get_keyboard_shortcuts
 */
export function useNetworkIdleEvents(
  context: Pick<
    UIBridgeEventContext,
    "bridgeRef" | "sendResponse" | "networkTrackerRef" | "idleDetectorRef"
  >,
) {
  const { sendResponse, networkTrackerRef, idleDetectorRef } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      switch (type) {
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
          return true;
        }

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
          return true;
        }

        case "get_idle_signal": {
          const { CompositeIdleDetector } = await import("ui-bridge");
          if (!idleDetectorRef.current) {
            idleDetectorRef.current = CompositeIdleDetector.create();
          }
          const signalName = (payload.params?.signal as string) ?? "";
          const signal = idleDetectorRef.current.getSignal(signalName);
          if (signal) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { signal: signalName, ...signal.getStatus() },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Unknown idle signal: ${signalName}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "wait_for_idle_signal": {
          const { CompositeIdleDetector } = await import("ui-bridge");
          if (!idleDetectorRef.current) {
            idleDetectorRef.current = CompositeIdleDetector.create();
          }
          const signalName2 = (payload.params?.signal as string) ?? "";
          const timeout2 = (payload.params?.timeout as number) ?? 30000;
          const signal2 = idleDetectorRef.current.getSignal(signalName2);
          if (signal2) {
            const status = await signal2.waitForIdle({ timeout: timeout2 });
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { signal: signalName2, ...status },
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Unknown idle signal: ${signalName2}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "wait_for_targets": {
          const { CompositeIdleDetector } = await import("ui-bridge");
          if (!idleDetectorRef.current) {
            idleDetectorRef.current = CompositeIdleDetector.create();
          }
          const targets = (payload.params?.targets as string[]) ?? [];
          const timeout3 = (payload.params?.timeout as number) ?? 30000;
          // Wait for all target signals concurrently with a shared deadline
          // so total time never exceeds timeout_ms regardless of target count.
          const deadline = Date.now() + timeout3;
          const results: Record<string, unknown> = {};
          const settled = await Promise.allSettled(
            targets.map(async (target) => {
              const signal = idleDetectorRef.current!.getSignal(target);
              if (!signal) return { target, status: null };
              const remaining = Math.max(0, deadline - Date.now());
              const status = await signal.waitForIdle({ timeout: remaining });
              return { target, status };
            }),
          );
          for (const outcome of settled) {
            if (outcome.status === "fulfilled" && outcome.value.status !== null) {
              results[outcome.value.target] = outcome.value.status;
            } else if (outcome.status === "rejected") {
              // Record the error for this target
              const errMsg =
                outcome.reason instanceof Error ? outcome.reason.message : String(outcome.reason);
              results["_error"] = errMsg;
            }
          }
          await sendResponse({
            requestId,
            type,
            success: true,
            data: results,
            timestamp: Date.now(),
          });
          return true;
        }

        case "wait_for_navigation_complete": {
          const navTimeout = (payload.params?.timeout as number) ?? 30000;
          const since = (payload.params?.since as number) ?? 0;

          const uiBridgeGlobalNav = getUIBridgeGlobal();
          const navTracker = uiBridgeGlobalNav?.navigationTracker as
            | {
                lastCompleteNavigation: { completedAt: number; url: string; routeKey: string } | null;
                onNavigationComplete: (
                  listener: (data: { completedAt: number; url: string; routeKey: string }) => void,
                ) => () => void;
              }
            | undefined;

          // Check if a navigation already completed since the requested timestamp
          const lastNav = navTracker?.lastCompleteNavigation;
          if (lastNav && lastNav.completedAt >= since) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { completed: true, ...lastNav },
              timestamp: Date.now(),
            });
            return true;
          }

          // Wait for a navigation-complete event or fall back to idle at timeout/2
          const result = await new Promise<Record<string, unknown>>((resolve) => {
            let settled = false;
            let unsub: (() => void) | undefined;

            const settle = (data: Record<string, unknown>) => {
              if (settled) return;
              settled = true;
              if (unsub) unsub();
              resolve(data);
            };

            // Subscribe to navigation-complete events if tracker is available
            if (navTracker?.onNavigationComplete) {
              unsub = navTracker.onNavigationComplete((data) => {
                settle({ completed: true, ...data });
              });
            }

            // Fallback: at timeout/2, try idle-based detection
            const fallbackId = setTimeout(async () => {
              if (settled) return;
              try {
                const { CompositeIdleDetector } = await import("ui-bridge");
                if (!idleDetectorRef.current) {
                  idleDetectorRef.current = CompositeIdleDetector.create();
                }
                await idleDetectorRef.current.waitForIdle({
                  timeout: Math.max(1000, navTimeout / 2),
                  minStableMs: 500,
                });
                settle({
                  completed: true,
                  method: "idle-fallback",
                  url: typeof window !== "undefined" ? window.location.href : "",
                  completedAt: Date.now(),
                });
              } catch {
                // Let the final timeout handle it
              }
            }, navTimeout / 2);

            // Final timeout — give up
            setTimeout(() => {
              clearTimeout(fallbackId);
              settle({
                completed: false,
                timedOut: true,
                url: typeof window !== "undefined" ? window.location.href : "",
                elapsed: navTimeout,
              });
            }, navTimeout);
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

        case "wait_for_element_stable": {
          const elId = (payload.params?.elementId as string) ?? payload.elementId ?? "";
          if (!elId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          // Look up element from the bridge registry via the global
          const uiBridgeGlobal = getUIBridgeGlobal();
          const registry = uiBridgeGlobal?.registry as
            | { getElement?: (id: string) => { element: Element } | null }
            | undefined;
          const registered = registry?.getElement?.(elId);
          const domElement = registered?.element;

          if (!domElement || !(domElement instanceof HTMLElement)) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found or not an HTMLElement: ${elId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          const { waitForElementStable } = await import("ui-bridge/idle");
          const result = await waitForElementStable(domElement, {
            quietMs: (payload.params?.quietMs as number) ?? 500,
            timeout: (payload.params?.timeout as number) ?? 5000,
            observeAttributes: (payload.params?.observeAttributes as boolean) ?? true,
            observeSubtree: (payload.params?.observeSubtree as boolean) ?? false,
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
          return true;
        }

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
          return true;
        }

        default:
          return false;
      }
    },
    [sendResponse, networkTrackerRef, idleDetectorRef],
  );
}
