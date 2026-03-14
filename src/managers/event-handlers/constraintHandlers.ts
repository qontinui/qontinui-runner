/**
 * Constraint Handlers
 *
 * Handles constraint evaluation result events:
 * - constraint-results: Results from the constraint engine after an agentic phase
 *
 * Note: The ConstraintResultsWidget listens directly to Tauri IPC events
 * (same pattern as ConvergenceWidget). This handler provides logging and
 * integration with the EventRouter system for other consumers.
 */

import { logManager } from "../index";
import type { HandlerSetupFunction } from "./types";
import type { EventPayload } from "../../types/eventPayloads";

/**
 * Setup constraint-related event handlers
 */
export const setupConstraintHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter } = context;
  const unsubscribers: Array<() => void> = [];

  // Handler for "constraint-results" event
  unsubscribers.push(
    eventRouter.subscribe("constraint-results", (payload: EventPayload) => {
      const data = payload.data as
        | {
            task_run_id?: string;
            iteration?: number;
            summary?: string;
            has_blocking?: boolean;
            results?: unknown[];
          }
        | undefined;
      const summary = data?.summary || "Constraint evaluation completed";
      const hasBlocking = data?.has_blocking || false;
      const iteration = data?.iteration;

      console.log(
        `[CONSTRAINT_HANDLER] constraint-results event received: iteration=${iteration}, has_blocking=${hasBlocking}`,
      );

      if (hasBlocking) {
        logManager.addLog("warning", `Constraint violations (iteration ${iteration}): ${summary}`);
      } else {
        logManager.addLog("info", `Constraints passed (iteration ${iteration}): ${summary}`);
      }
    }),
  );

  return unsubscribers;
};
