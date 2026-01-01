/**
 * Event Handlers - Index
 *
 * Re-exports all event handler modules and provides the main setup function.
 */

import type { EventRouter } from "../EventRouter";
import type { ExecutionContextActions, HandlerContext, HandlerSetupFunction } from "./types";
import { setupExecutionHandlers } from "./executionHandlers";
import { setupImageHandlers } from "./imageHandlers";
import { setupAiOutputHandlers } from "./aiOutputHandlers";
import { setupReportingHandlers } from "./reportingHandlers";

// Re-export types
export type { ExecutionContextActions, HandlerContext, HandlerSetupFunction } from "./types";

// Re-export individual handler setup functions for granular usage
export { setupExecutionHandlers } from "./executionHandlers";
export { setupImageHandlers } from "./imageHandlers";
export {
  setupAiOutputHandlers,
  getCurrentAiActionId,
  resetAiSessionState,
} from "./aiOutputHandlers";
export { setupReportingHandlers } from "./reportingHandlers";

/**
 * All handler setup functions in order
 */
const allHandlerSetups: HandlerSetupFunction[] = [
  setupExecutionHandlers,
  setupImageHandlers,
  setupAiOutputHandlers,
  setupReportingHandlers,
];

/**
 * Setup all event handlers for the EventRouter
 * @param eventRouter - The EventRouter instance
 * @param executionActions - Actions from ExecutionContext
 * @returns Cleanup function to unsubscribe all handlers
 */
export function setupEventHandlers(
  eventRouter: EventRouter,
  executionActions: ExecutionContextActions,
): () => void {
  const context: HandlerContext = {
    eventRouter,
    executionActions,
  };

  // Collect all unsubscribe functions from all handler modules
  const allUnsubscribers: Array<() => void> = [];

  for (const setupFn of allHandlerSetups) {
    const unsubscribers = setupFn(context);
    allUnsubscribers.push(...unsubscribers);
  }

  console.log("[EVENT_HANDLERS] All event handlers registered");

  // Return cleanup function that unsubscribes all handlers
  return () => {
    console.log("[EVENT_HANDLERS] Cleaning up all event handlers");
    allUnsubscribers.forEach((unsub) => unsub());
  };
}
