/**
 * useSpecExecutionHandler
 *
 * React hook that listens for Tauri `spec-execute-request` events from the Rust backend
 * and responds with spec execution results via `spec-execute-response` events.
 *
 * Architecture:
 * ```
 * Rust Step Executor (spec handler)
 *     | Tauri emit("spec-execute-request", payload)
 *     v
 * This Hook (useSpecExecutionHandler)
 *     | Discovers elements via bridge.discover() (DOM scan)
 *     | Runs SpecExecutor.executeGroup()
 *     v
 * Tauri emit("spec-execute-response", response)
 *     | Rust side receives via oneshot channel
 *     v
 * Rust Step Executor -> StepHandlerResult
 * ```
 */

import { useEffect, useCallback, useRef } from "react";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { useUIBridge } from "ui-bridge";
import type { SpecGroup, SpecGroupResult, SpecExecutionOptions } from "@qontinui/ui-bridge/specs";
import { executeSpecGroup, externalToDiscovered } from "../lib/spec-execution";
import { IpcArtifactStore } from "@qontinui/ui-bridge/artifacts";
import type { ExternalElement } from "../types/ui-bridge-types";
import { getErrorMessage } from "@/lib/utils";

/** Module-level singleton so all spec runs share one IPC-backed store. */
const artifactStore = new IpcArtifactStore();

/**
 * Payload for spec execution requests from Rust
 */
interface SpecExecuteRequestPayload {
  requestId: string;
  group: SpecGroup;
  element_source: "control" | "external";
  options?: SpecExecutionOptions;
  /** External elements provided by Rust */
  elements?: ExternalElement[];
}

/**
 * Payload for spec execution responses back to Rust
 */
interface SpecExecuteResponsePayload {
  requestId: string;
  success: boolean;
  result?: SpecGroupResult;
  error?: string;
}

/**
 * Hook that handles spec execution requests from Tauri events.
 *
 * Must be used inside UIBridgeProvider to access element discovery.
 */
export function useSpecExecutionHandler(): void {
  const bridge = useUIBridge();
  const bridgeRef = useRef(bridge);

  // Keep bridge ref updated to avoid stale closures
  useEffect(() => {
    bridgeRef.current = bridge;
  }, [bridge]);

  /**
   * Send a response back to the Rust backend
   */
  const sendResponse = useCallback(async (response: SpecExecuteResponsePayload) => {
    try {
      await emit("spec-execute-response", response);
    } catch (error) {
      console.error("[SpecExecutionHandler] Failed to emit response:", error);
    }
  }, []);

  /**
   * Handle incoming spec execution requests
   */
  const handleRequest = useCallback(
    async (payload: SpecExecuteRequestPayload) => {
      const { requestId, group, element_source, options, elements: externalElements } = payload;
      const currentBridge = bridgeRef.current;

      try {
        if (element_source === "control") {
          if (!currentBridge.available) {
            await sendResponse({
              requestId,
              success: false,
              error: "UI Bridge is not available",
            });
            return;
          }

          // Discover elements by scanning the DOM (not from registry which may be empty)
          const findResult = await currentBridge.discover({ includeHidden: false });
          const discoveredElements = findResult.elements;

          // Execute the spec group against discovered elements
          const result = await executeSpecGroup(group, discoveredElements, {
            ...options,
            artifactStore,
            specId: group.id,
          });

          await sendResponse({
            requestId,
            success: true,
            result,
          });
        } else if (element_source === "external") {
          // External source: elements are provided in the payload by Rust
          if (!externalElements || externalElements.length === 0) {
            await sendResponse({
              requestId,
              success: false,
              error:
                "No external elements provided. Ensure an SDK app is connected and has elements.",
            });
            return;
          }

          // Convert ExternalElement[] to DiscoveredElement[]
          const discoveredElements = externalElements.map(externalToDiscovered);

          // Execute the spec group against external elements
          const result = await executeSpecGroup(group, discoveredElements, {
            ...options,
            artifactStore,
            specId: group.id,
          });

          await sendResponse({
            requestId,
            success: true,
            result,
          });
        } else {
          await sendResponse({
            requestId,
            success: false,
            error: `Unknown element source "${element_source}"`,
          });
        }
      } catch (error) {
        console.error(`[SpecExecutionHandler] Error executing spec group:`, error);
        await sendResponse({
          requestId,
          success: false,
          error: getErrorMessage(error),
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
        unlisten = await listen<SpecExecuteRequestPayload>("spec-execute-request", (event) => {
          if (!isMounted) return;

          // Handle the request asynchronously
          handleRequest(event.payload).catch((error) => {
            console.error("[SpecExecutionHandler] Unhandled error in request handler:", error);
          });
        });
      } catch (error) {
        console.error("[SpecExecutionHandler] Failed to set up listener:", error);
      }
    };

    setupListener();

    return () => {
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
export function SpecExecutionHandler(): null {
  useSpecExecutionHandler();
  return null;
}
