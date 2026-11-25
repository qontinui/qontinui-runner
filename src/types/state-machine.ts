/**
 * State Machine Command Types and Wrappers
 *
 * This module provides TypeScript types and helper functions for interacting
 * with the state machine via Tauri commands: resolving a workflow's initial states.
 *
 * @module state-machine
 */

import { invoke } from "@tauri-apps/api/core";
import { describeThrown } from "@/lib/utils";

// ============================================================================
// Initial States Types
// ============================================================================

/**
 * Source of initial states configuration.
 *
 * - "defaults": States with is_initial=true in the state machine definition
 * - "workflow": Initial states defined on the workflow (initialStateIds)
 * - "override": Session-only override from the runner UI
 */
export type InitialStatesSource = "defaults" | "workflow" | "override";

/**
 * Resolved initial states with source tracking.
 *
 * This represents the final resolved initial states that will be used
 * for workflow execution, along with metadata about where they came from.
 */
export interface ResolvedInitialStates {
  /**
   * Array of state IDs that will be active when the workflow starts.
   */
  stateIds: string[];

  /**
   * The source of these initial states (defaults, workflow, or override).
   */
  source: InitialStatesSource;

  /**
   * State information with names for UI display.
   */
  states?: Array<{ id: string; name: string }>;

  /**
   * The workflow ID these states were resolved for.
   */
  workflowId?: string;
}

/**
 * Result of getting resolved initial states from Tauri command.
 */
export interface ResolvedInitialStatesResult {
  /**
   * Whether the query executed successfully.
   */
  success: boolean;

  /**
   * Array of state IDs that will be active when the workflow starts.
   */
  stateIds: string[];

  /**
   * The source of these initial states.
   */
  source: InitialStatesSource;

  /**
   * State information with names for UI display.
   */
  states: Array<{ id: string; name: string }>;

  /**
   * The workflow ID these states were resolved for.
   */
  workflowId: string;

  /**
   * Error message if the query failed.
   */
  error?: string;
}

// ============================================================================
// Command Wrapper Functions
// ============================================================================

/**
 * Get the resolved initial states for a workflow.
 *
 * Returns the initial states that would be used for execution, along with
 * the source of those states (defaults, workflow, or override).
 *
 * This is useful for the UI to display current initial states before execution
 * without actually starting a workflow.
 *
 * @param workflowId - The workflow ID to get initial states for
 * @returns Promise resolving to the resolved initial states result
 *
 * @example
 * ```typescript
 * const result = await getResolvedInitialStates("main_workflow");
 * if (result.success) {
 *   console.log("Initial states:", result.stateIds);
 *   console.log("Source:", result.source);
 *   result.states.forEach(s => {
 *     console.log(`- ${s.name} (${s.id})`);
 *   });
 * }
 * ```
 */
export async function getResolvedInitialStates(
  workflowId: string,
): Promise<ResolvedInitialStatesResult> {
  try {
    const response = await invoke<{
      success: boolean;
      data?: {
        stateIds: string[];
        source: InitialStatesSource;
        states: Array<{ id: string; name: string }>;
        workflowId: string;
      };
      message?: string;
    }>("get_resolved_initial_states", { workflowId });

    if (response.success && response.data) {
      return {
        success: true,
        stateIds: response.data.stateIds,
        source: response.data.source,
        states: response.data.states,
        workflowId: response.data.workflowId,
      };
    }

    return {
      success: false,
      stateIds: [],
      source: "defaults",
      states: [],
      workflowId,
      error: response.message || "Failed to get initial states",
    };
  } catch (error) {
    return {
      success: false,
      stateIds: [],
      source: "defaults",
      states: [],
      workflowId,
      error: describeThrown(error, "Failed to get initial states"),
    };
  }
}
