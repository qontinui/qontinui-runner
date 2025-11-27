/**
 * EventHandlers
 *
 * Event handler functions that process events routed through EventRouter.
 * Each handler is responsible for updating the appropriate manager/state.
 */

import { logManager, actionLogManager, windowManager } from "./index";

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

    // Delegate image recognition processing to LogManager
    logManager.processImageRecognitionData(data);
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
