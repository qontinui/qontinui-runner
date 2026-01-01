/**
 * Event Handlers - Shared Types
 *
 * Type definitions shared across all event handler modules.
 */

import type { EventRouter } from "../EventRouter";

/**
 * Execution context actions available to event handlers
 */
export interface ExecutionContextActions {
  setPythonStatus: (status: "stopped" | "running") => void;
  setConfigLoaded: (loaded: boolean) => void;
  setExecutionActive: (active: boolean) => void;
}

/**
 * Handler context passed to each handler module's setup function
 */
export interface HandlerContext {
  eventRouter: EventRouter;
  executionActions: ExecutionContextActions;
}

/**
 * Handler setup function signature
 * Returns an array of unsubscribe functions
 */
export type HandlerSetupFunction = (context: HandlerContext) => Array<() => void>;
