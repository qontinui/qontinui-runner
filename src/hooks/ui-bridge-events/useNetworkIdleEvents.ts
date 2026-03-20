import { useCallback } from "react";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";

/**
 * Handles: get_network_requests, get_network_requests_in_flight,
 *          get_idle_status, wait_for_idle, diagnose_stuck_screen, get_keyboard_shortcuts
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
