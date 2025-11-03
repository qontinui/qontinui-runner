/**
 * EventHandlers
 *
 * Event handler functions that process events routed through EventRouter.
 * Each handler is responsible for updating the appropriate manager/state.
 */

import { logManager, actionLogManager, windowManager } from "./index";
import { ImageRecognitionEntry } from "./LogManager";

interface ExecutionContextActions {
  setPythonStatus: (status: "stopped" | "running") => void;
  setConfigLoaded: (loaded: boolean) => void;
  setExecutionActive: (active: boolean) => void;
}

/**
 * Setup event handlers for the EventRouter
 * @param eventRouter - The EventRouter instance
 * @param executionActions - Actions from ExecutionContext
 * @returns Cleanup function to unsubscribe all handlers
 */
export function setupEventHandlers(
  eventRouter: any,
  executionActions: ExecutionContextActions
): () => void {
  const { setPythonStatus, setConfigLoaded, setExecutionActive } = executionActions;

  // Collect unsubscribe functions
  const unsubscribers: Array<() => void> = [];

  // Handler for "ready" event
  unsubscribers.push(eventRouter.subscribe("ready", () => {
    console.log("[EVENT_HANDLER] ready event received");
    setPythonStatus("running");
    logManager.addLog("info", "Python executor ready");
  }));

  // Handler for "config_loaded" event
  unsubscribers.push(eventRouter.subscribe("config_loaded", () => {
    console.log("[EVENT_HANDLER] config_loaded event received");
    setConfigLoaded(true);
    logManager.addLog("info", "Configuration loaded successfully");
  }));

  // Handler for "execution_started" event
  unsubscribers.push(eventRouter.subscribe("execution_started", () => {
    console.log("[EVENT_HANDLER] execution_started event received");
    setExecutionActive(true);
  }));

  // Handler for "execution_completed" event
  unsubscribers.push(eventRouter.subscribe("execution_completed", () => {
    console.log("[EVENT_HANDLER] execution_completed event received");
    setExecutionActive(false);
    logManager.addLog("success", "Execution completed successfully");

    // Restore window if it was auto-minimized
    windowManager.restoreIfMinimized();
  }));

  // Handler for "error" event
  unsubscribers.push(eventRouter.subscribe("error", (payload: any) => {
    console.log("[EVENT_HANDLER] error event received:", payload.data);
    const errorMessage = payload.data?.message || "Unknown error occurred";
    logManager.addLog("error", errorMessage);
  }));

  // Handler for "log" event
  unsubscribers.push(eventRouter.subscribe("log", (payload: any) => {
    console.log("[EVENT_HANDLER] log event received");
    const level = payload.data?.level || "info";
    const message = payload.data?.message || "";
    logManager.addLog(level, message);
  }));

  // Handler for "tree_event" event
  unsubscribers.push(eventRouter.subscribe("tree_event", () => {
    console.log("[EVENT_HANDLER] tree_event received, triggering action log refresh");
    actionLogManager.triggerRefresh();
  }));

  // Handler for "image_recognition" event
  unsubscribers.push(eventRouter.subscribe("image_recognition", (payload: any) => {
    console.log("[EVENT_HANDLER] image_recognition event received");
    const data = payload.data;

    if (!data) {
      console.warn("[EVENT_HANDLER] image_recognition event has no data");
      return;
    }

    // Format timestamps
    const timestamp = new Date().toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });

    let screenshotTimestamp: string | undefined;
    if (data.screenshot_timestamp) {
      const screenshotDate = new Date(data.screenshot_timestamp * 1000);
      screenshotTimestamp = screenshotDate.toLocaleTimeString("en-US", {
        hour12: false,
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      });
    }

    // Format location - backend can send location as string "(x, y)" or object
    let location: { x: number; y: number; width: number; height: number } | undefined;
    if (data.location) {
      if (typeof data.location === "string") {
        // Parse "(x, y)" format
        const match = data.location.match(/\((\d+),\s*(\d+)\)/);
        if (match) {
          location = {
            x: parseInt(match[1]),
            y: parseInt(match[2]),
            width: 0,
            height: 0,
          };
        }
      } else if (typeof data.location === "object") {
        location = {
          x: data.location.x,
          y: data.location.y,
          width: data.location.width || 0,
          height: data.location.height || 0,
        };
      }
    }

    // Calculate gap and percent off - use data from backend if available
    let gap: number | undefined = data.gap;
    let percentOff: number | undefined = data.percent_off;
    let bestMatchLocation: string | undefined = data.best_match_location;

    // Fallback: calculate from debug data if not provided
    if (!data.found && !gap && data.debug?.top_matches && data.debug.top_matches.length > 0) {
      const bestMatch = data.debug.top_matches[0];
      gap = data.threshold - bestMatch.confidence;
      percentOff = (gap / data.threshold) * 100;
      bestMatchLocation = `(${bestMatch.location.x}, ${bestMatch.location.y})`;
    }

    // Extract node ID - hierarchy is an object, need to extract workflow_name
    let nodeId = "unknown";
    if (data.node_id) {
      nodeId = data.node_id;
    } else if (data.hierarchy && typeof data.hierarchy === "object") {
      // Hierarchy is {nesting_level, parent_id, workflow_name}
      nodeId = data.hierarchy.workflow_name || data.hierarchy.parent_id || "unknown";
    } else if (typeof data.hierarchy === "string") {
      nodeId = data.hierarchy;
    }

    const entry: ImageRecognitionEntry = {
      id: `img-${Date.now()}-${Math.random()}`,
      timestamp,
      screenshotTimestamp,
      node: nodeId,
      template: data.template_name || data.image_path || "unknown", // Backend sends image_path
      confidence: data.confidence || 0,
      found: data.found || false,
      threshold: data.threshold || 0,
      location,
      gap,
      percentOff,
      bestMatchLocation,
      screenshotPath: data.screenshot_path,
      screenshotData: data.screenshot_base64, // Base64 screenshot from library
      visualDebugImage: data.visual_debug_image, // Annotated screenshot with colored boxes
      templatePath: data.template_path,
      imageData: data.image_data, // Base64 image data if available
      debug: data.debug, // Debug data with top_matches
    };

    logManager.addImageLog(entry);
  }));

  // Handler for "action_started" event
  unsubscribers.push(eventRouter.subscribe("action_started", (payload: any) => {
    const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
    logManager.addLog("debug", `Action started: ${actionType}`);
  }));

  // Handler for "action_completed" event
  unsubscribers.push(eventRouter.subscribe("action_completed", (payload: any) => {
    const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
    logManager.addLog("debug", `Action completed: ${actionType}`);
  }));

  console.log("[EVENT_HANDLERS] All event handlers registered");

  // Return cleanup function that unsubscribes all handlers
  return () => {
    console.log("[EVENT_HANDLERS] Cleaning up all event handlers");
    unsubscribers.forEach(unsub => unsub());
  };
}
