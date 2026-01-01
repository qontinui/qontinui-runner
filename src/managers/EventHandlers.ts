/**
 * EventHandlers
 *
 * Event handler functions that process events routed through EventRouter.
 * Each handler is responsible for updating the appropriate manager/state.
 *
 * NOTE: This file is maintained for backward compatibility.
 * The actual implementation has been refactored into domain-focused modules
 * in the ./event-handlers/ directory:
 * - executionHandlers.ts - Workflow execution lifecycle events
 * - imageHandlers.ts - Image recognition events
 * - aiOutputHandlers.ts - AI output streaming events
 * - reportingHandlers.ts - Logging and reporting events
 */

// Re-export everything from the new modular structure
export {
  setupEventHandlers,
  type ExecutionContextActions,
  type HandlerContext,
  type HandlerSetupFunction,
  // Individual handler setup functions for granular usage
  setupExecutionHandlers,
  setupImageHandlers,
  setupAiOutputHandlers,
  setupReportingHandlers,
  // AI session utilities
  getCurrentAiActionId,
  resetAiSessionState,
} from "./event-handlers";
