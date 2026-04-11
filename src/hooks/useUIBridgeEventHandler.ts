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

import { useEffect, useLayoutEffect, useCallback, useRef } from "react";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { useUIBridge } from "ui-bridge";
import type { StyleGuideConfig } from "ui-bridge";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";

const log = createLogger("UIBridgeEventHandler");

import {
  useControlEvents,
  useDiscoveryEvents,
  usePageEvents,
  useDesignEvents,
  useChangeTrackingEvents,
  useDebugInspectEvents,
  useNetworkIdleEvents,
  useAISearchEvents,
  useWorkflowEvents,
  useMediaEvents,
  useAnnotationEvents,
} from "./ui-bridge-events";

import type { UIBridgeRequestPayload, UIBridgeResponsePayload } from "./ui-bridge-events/types";
import { httpSendResponse, httpSendPong } from "./ui-bridge-events/utils";

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

  // Keep bridge ref updated via useLayoutEffect to minimize the gap between
  // rerender and ref update. IPC events arriving in the gap will use the
  // previous bridge reference, which is acceptable.
  useLayoutEffect(() => {
    bridgeRef.current = bridge;
  }, [bridge]);

  /**
   * Send a response back to the Rust backend
   */
  const sendResponse = useCallback(async (response: UIBridgeResponsePayload) => {
    // Send via HTTP first (primary). The Tauri emit path suffers from
    // double-serialization: Tauri serializes the payload to JSON for IPC,
    // then the Rust listener deserializes via serde_json::from_str(event.payload()).
    // This can mangle nested data structures (elements arrays become empty
    // or truncated). The HTTP path sends raw JSON directly, avoiding this issue.
    //
    // Only fall back to Tauri emit if HTTP fails (e.g., API port not bound yet).
    const httpOk = await httpSendResponse(response);
    if (httpOk) {
      log.debug(`HTTP sent response for ${response.type}:`, response.requestId);
      return;
    }
    // HTTP failed — fall back to Tauri emit
    try {
      await emit("ui-bridge-response", response);
      log.debug(`Tauri emit sent response for ${response.type}:`, response.requestId);
    } catch {
      // Both channels failed — response is lost
      console.error(`[UIBridgeEventHandler] Both HTTP and Tauri emit failed for ${response.type}:`, response.requestId);
    }
  }, []);

  // Build the shared context for sub-hooks
  const context = {
    bridgeRef,
    sendResponse,
    loadedStyleGuideRef,
    changeTrackerRef,
    networkTrackerRef,
    idleDetectorRef,
  };

  // Initialize sub-hook handlers
  const handleControl = useControlEvents(context);
  const handleDiscovery = useDiscoveryEvents(context);
  const handlePage = usePageEvents(context);
  const handleDesign = useDesignEvents(context);
  const handleChangeTracking = useChangeTrackingEvents(context);
  const handleDebugInspect = useDebugInspectEvents(context);
  const handleNetworkIdle = useNetworkIdleEvents(context);
  const handleAISearch = useAISearchEvents(context);
  const handleWorkflow = useWorkflowEvents(context);
  const handleMedia = useMediaEvents(context);
  const handleAnnotations = useAnnotationEvents(context);

  /**
   * Handle incoming UI Bridge requests
   */
  const handleRequest = useCallback(
    async (payload: UIBridgeRequestPayload) => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      log.debug(`Received request: ${type}`, requestId);

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
        // Chain sub-hooks: first one to return true wins
        const handlers = [
          handleControl,
          handleDiscovery,
          handlePage,
          handleDesign,
          handleChangeTracking,
          handleDebugInspect,
          handleNetworkIdle,
          handleAISearch,
          handleWorkflow,
          handleMedia,
          handleAnnotations,
        ];

        for (const handler of handlers) {
          const handled = await handler(payload);
          if (handled) return;
        }

        // No handler matched — unknown request type
        await sendResponse({
          requestId,
          type,
          success: false,
          error: `Unknown request type: ${type}`,
          timestamp: Date.now(),
        });
      } catch (error) {
        console.error(`[UIBridgeEventHandler] Error handling ${type}:`, error);
        await sendResponse({
          requestId,
          type,
          success: false,
          error: getErrorMessage(error),
          timestamp: Date.now(),
        });
      }
    },
    [
      sendResponse,
      handleControl,
      handleDiscovery,
      handlePage,
      handleDesign,
      handleChangeTracking,
      handleDebugInspect,
      handleNetworkIdle,
      handleAISearch,
      handleWorkflow,
      handleMedia,
      handleAnnotations,
    ],
  );

  // Set up the Tauri event listener
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        log.debug("Setting up ui-bridge-request listener");

        unlisten = await listen<UIBridgeRequestPayload>("ui-bridge-request", (event) => {
          if (!isMounted) {
            log.debug("Component unmounted, ignoring event");
            return;
          }

          const payload = event.payload;
          log.debug("Received event:", payload.type, payload.requestId);

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

        log.debug("Listener set up successfully");

        // Signal readiness immediately rather than waiting for next ping cycle.
        // This unblocks any Rust-side requests waiting on the readiness gate.
        httpSendPong().catch(() => {});

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
      log.debug("Page unloading, cleaning up");
    };
    window.addEventListener("beforeunload", handleBeforeUnload);

    setupListener();

    return () => {
      log.debug("Cleaning up listener");
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
