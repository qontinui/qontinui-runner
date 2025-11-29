/**
 * Type definitions index
 *
 * Central export point for all type definitions used in the application.
 */

// Tree event types for hierarchical action logging
export type {
  TreeEventData,
  DisplayNode,
  TreeNode,
  NodeMetadata,
  NodeType,
  NodeStatus,
  TreeEventType,
  PathElement,
} from "./treeEvents";

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

// Display profile types for the new architecture
export type { ActionLogViewData, ActionLogEntry, CommandResponse } from "./displayProfile";
