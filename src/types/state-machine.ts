/**
 * State Machine Command Types and Wrappers
 *
 * This module provides TypeScript types and helper functions for interacting
 * with the state machine via Tauri commands. It offers type-safe wrappers
 * around the Python/Rust bridge for state navigation and transition execution.
 *
 * @module state-machine
 */

import { invoke } from "@tauri-apps/api/core";

// ============================================================================
// Type Definitions
// ============================================================================

/**
 * Result of executing a state transition.
 *
 * Contains information about the transition execution including success status,
 * the states that were activated/deactivated, and any error messages.
 */
export interface TransitionExecutionResult {
  /**
   * Whether the transition executed successfully.
   */
  success: boolean;

  /**
   * ID of the transition that was executed.
   */
  transition_id: string;

  /**
   * List of state IDs that are currently active after transition.
   */
  active_states: string[];

  /**
   * Error message if the transition failed.
   * Undefined if success is true.
   */
  error?: string;
}

/**
 * Result of navigating to one or more states.
 *
 * Contains the navigation path taken, current active states, and success status.
 */
export interface NavigationResult {
  /**
   * Whether the navigation completed successfully.
   */
  success: boolean;

  /**
   * The path of states traversed during navigation.
   * Empty array if navigation failed or no path was needed.
   */
  path: string[];

  /**
   * List of state IDs that are currently active after navigation.
   */
  active_states: string[];

  /**
   * ID of the target state (single state navigation).
   * Undefined for multi-state navigation.
   */
  target_state?: string;

  /**
   * Detailed results for each state in multi-state navigation.
   * Undefined for single state navigation.
   */
  results?: NavigationResult[];

  /**
   * Error message if navigation failed.
   * Undefined if success is true.
   */
  error?: string;
}

/**
 * Result of querying currently active states.
 *
 * Provides information about which states are currently active in the state machine.
 */
export interface ActiveStatesResult {
  /**
   * Whether the query executed successfully.
   */
  success: boolean;

  /**
   * List of currently active state IDs.
   */
  active_states: string[];

  /**
   * The primary current state (if single-state mode).
   * May be null if no state is active or in multi-state mode.
   */
  current_state?: string | null;

  /**
   * History of previously active states.
   * Most recent states appear first.
   */
  state_history?: string[];

  /**
   * Error message if the query failed.
   * Undefined if success is true.
   */
  error?: string;
}

/**
 * Information about a single transition.
 */
export interface TransitionInfo {
  /**
   * Unique identifier for the transition.
   */
  id: string;

  /**
   * ID of the source state.
   */
  from_state: string;

  /**
   * ID of the destination state.
   * May be null for transitions without a fixed destination.
   */
  to_state: string | null;

  /**
   * List of workflow IDs that can trigger this transition.
   */
  workflows: string[];
}

/**
 * Result of querying available transitions from current state.
 *
 * Provides information about which transitions can be executed from the current state.
 */
export interface AvailableTransitionsResult {
  /**
   * Whether the query executed successfully.
   */
  success: boolean;

  /**
   * List of available transitions.
   * Empty array if no transitions are available or query failed.
   */
  transitions: TransitionInfo[];

  /**
   * The current state these transitions are available from.
   * May be undefined if no current state.
   */
  current_state?: string;

  /**
   * Informational message (e.g., "No current state").
   * Present when query succeeds but has no results.
   */
  message?: string;

  /**
   * Error message if the query failed.
   * Undefined if success is true.
   */
  error?: string;
}

// ============================================================================
// Command Wrapper Functions
// ============================================================================

/**
 * Execute a specific state transition by ID.
 *
 * This command triggers a transition in the state machine, executing any
 * associated workflows and updating the active state set.
 *
 * @param transitionId - The unique identifier of the transition to execute
 * @returns Promise resolving to the execution result
 *
 * @example
 * ```typescript
 * const result = await executeTransition("login_to_dashboard");
 * if (result.success) {
 *   console.log("Active states:", result.active_states);
 * } else {
 *   console.error("Transition failed:", result.error);
 * }
 * ```
 */
export async function executeTransition(
  transitionId: string
): Promise<TransitionExecutionResult> {
  try {
    const result = await invoke<TransitionExecutionResult>("execute_transition", {
      transitionId,
    });
    return result;
  } catch (error) {
    return {
      success: false,
      transition_id: transitionId,
      active_states: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Navigate to a target state by finding and executing the optimal path.
 *
 * The state machine will automatically determine the best path from the current
 * state(s) to the target state and execute all necessary transitions.
 *
 * @param stateId - The unique identifier of the target state
 * @returns Promise resolving to the navigation result
 *
 * @example
 * ```typescript
 * const result = await navigateToState("user_dashboard");
 * if (result.success) {
 *   console.log("Navigation path:", result.path);
 *   console.log("Now at states:", result.active_states);
 * } else {
 *   console.error("Navigation failed:", result.error);
 * }
 * ```
 */
export async function navigateToState(stateId: string): Promise<NavigationResult> {
  try {
    const result = await invoke<NavigationResult>("navigate_to_state", {
      stateId,
    });
    return result;
  } catch (error) {
    return {
      success: false,
      path: [],
      active_states: [],
      target_state: stateId,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Navigate to multiple target states simultaneously.
 *
 * In multi-state mode, this command will navigate to all specified states,
 * finding optimal paths for each and handling any conflicts.
 *
 * @param stateIds - Array of target state identifiers
 * @returns Promise resolving to the navigation result with detailed per-state results
 *
 * @example
 * ```typescript
 * const result = await navigateToMultipleStates(["email_open", "chat_open"]);
 * if (result.success) {
 *   console.log("All states reached:", result.active_states);
 *   result.results?.forEach((r, i) => {
 *     console.log(`State ${stateIds[i]}: ${r.success ? 'OK' : 'FAILED'}`);
 *   });
 * }
 * ```
 */
export async function navigateToMultipleStates(
  stateIds: string[]
): Promise<NavigationResult> {
  try {
    const result = await invoke<NavigationResult>("navigate_to_multiple_states", {
      stateIds,
    });
    return result;
  } catch (error) {
    return {
      success: false,
      path: [],
      active_states: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Get the currently active states in the state machine.
 *
 * This query returns which states are currently active, along with the
 * primary current state and state history.
 *
 * @returns Promise resolving to the active states result
 *
 * @example
 * ```typescript
 * const result = await getActiveStates();
 * if (result.success) {
 *   console.log("Active states:", result.active_states);
 *   console.log("Current state:", result.current_state);
 *   console.log("Recent history:", result.state_history);
 * }
 * ```
 */
export async function getActiveStates(): Promise<ActiveStatesResult> {
  try {
    const result = await invoke<ActiveStatesResult>("get_active_states");
    return result;
  } catch (error) {
    return {
      success: false,
      active_states: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Get all transitions available from the current state.
 *
 * This query returns information about which transitions can be executed
 * from the current state, including their destinations and triggering workflows.
 *
 * @returns Promise resolving to the available transitions result
 *
 * @example
 * ```typescript
 * const result = await getAvailableTransitions();
 * if (result.success) {
 *   console.log("Current state:", result.current_state);
 *   result.transitions.forEach(t => {
 *     console.log(`Transition ${t.id}: ${t.from_state} -> ${t.to_state}`);
 *   });
 * }
 * ```
 */
export async function getAvailableTransitions(): Promise<AvailableTransitionsResult> {
  try {
    const result = await invoke<AvailableTransitionsResult>("get_available_transitions");
    return result;
  } catch (error) {
    return {
      success: false,
      transitions: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

// ============================================================================
// Utility Functions
// ============================================================================

/**
 * Type guard to check if a state is currently active.
 *
 * @param stateId - State ID to check
 * @param activeStates - Active states result or array of state IDs
 * @returns True if the state is active
 *
 * @example
 * ```typescript
 * const activeStates = await getActiveStates();
 * if (isStateActive("dashboard", activeStates)) {
 *   console.log("Dashboard is active");
 * }
 * ```
 */
export function isStateActive(
  stateId: string,
  activeStates: ActiveStatesResult | string[]
): boolean {
  const states = Array.isArray(activeStates)
    ? activeStates
    : activeStates.active_states;
  return states.includes(stateId);
}

/**
 * Find a transition by ID in the available transitions list.
 *
 * @param transitionId - Transition ID to find
 * @param availableTransitions - Available transitions result
 * @returns The transition info if found, undefined otherwise
 *
 * @example
 * ```typescript
 * const transitions = await getAvailableTransitions();
 * const loginTransition = findTransitionById("login", transitions);
 * if (loginTransition) {
 *   console.log("Login goes to:", loginTransition.to_state);
 * }
 * ```
 */
export function findTransitionById(
  transitionId: string,
  availableTransitions: AvailableTransitionsResult
): TransitionInfo | undefined {
  return availableTransitions.transitions.find((t) => t.id === transitionId);
}

/**
 * Get all transitions that lead to a specific state.
 *
 * @param targetStateId - The destination state ID
 * @param availableTransitions - Available transitions result
 * @returns Array of transitions that go to the target state
 *
 * @example
 * ```typescript
 * const transitions = await getAvailableTransitions();
 * const pathsToDashboard = getTransitionsToState("dashboard", transitions);
 * console.log(`Found ${pathsToDashboard.length} ways to reach dashboard`);
 * ```
 */
export function getTransitionsToState(
  targetStateId: string,
  availableTransitions: AvailableTransitionsResult
): TransitionInfo[] {
  return availableTransitions.transitions.filter(
    (t) => t.to_state === targetStateId
  );
}

/**
 * Check if any state in a list is currently active.
 *
 * @param stateIds - Array of state IDs to check
 * @param activeStates - Active states result or array of state IDs
 * @returns True if any of the states is active
 *
 * @example
 * ```typescript
 * const activeStates = await getActiveStates();
 * if (isAnyStateActive(["logged_in", "authenticated"], activeStates)) {
 *   console.log("User is authenticated in some way");
 * }
 * ```
 */
export function isAnyStateActive(
  stateIds: string[],
  activeStates: ActiveStatesResult | string[]
): boolean {
  const states = Array.isArray(activeStates)
    ? activeStates
    : activeStates.active_states;
  return stateIds.some((id) => states.includes(id));
}

/**
 * Check if all states in a list are currently active.
 *
 * @param stateIds - Array of state IDs to check
 * @param activeStates - Active states result or array of state IDs
 * @returns True if all of the states are active
 *
 * @example
 * ```typescript
 * const activeStates = await getActiveStates();
 * if (areAllStatesActive(["logged_in", "email_open"], activeStates)) {
 *   console.log("User is logged in with email open");
 * }
 * ```
 */
export function areAllStatesActive(
  stateIds: string[],
  activeStates: ActiveStatesResult | string[]
): boolean {
  const states = Array.isArray(activeStates)
    ? activeStates
    : activeStates.active_states;
  return stateIds.every((id) => states.includes(id));
}
