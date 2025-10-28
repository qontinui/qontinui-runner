/**
 * Type definitions index
 *
 * Central export point for all type definitions used in the application.
 */

// Event types for hierarchical action logging
export type {
  HierarchyMetadata,
  ActionExecutionEvent,
  WorkflowEvent,
  WorkflowStartedEvent,
  WorkflowCompletedEvent,
  HierarchicalEvent,
} from "./events";

// State machine types and functions
export type {
  TransitionExecutionResult,
  NavigationResult,
  ActiveStatesResult,
  TransitionInfo,
  AvailableTransitionsResult,
} from "./state-machine";

export {
  executeTransition,
  navigateToState,
  navigateToMultipleStates,
  getActiveStates,
  getAvailableTransitions,
  isStateActive,
  findTransitionById,
  getTransitionsToState,
  isAnyStateActive,
  areAllStatesActive,
} from "./state-machine";
