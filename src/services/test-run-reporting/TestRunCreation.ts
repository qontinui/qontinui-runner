/**
 * TestRunCreation - Module for creating new test runs
 *
 * @deprecated This module is part of the deprecated TestRunReportingService.
 * Use ExecutionReportingService for new implementations.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CreateTestRunInput, CreateTestRunResponse, ActiveRunState } from "./types";

/**
 * Options for starting a test run
 */
export interface StartTestRunOptions {
  totalStates?: number;
  totalTransitions?: number;
  description?: string;
}

/**
 * Creates a new test run via the Rust backend
 *
 * @param projectId - The project ID
 * @param workflowId - The workflow ID (string, will be parsed to int)
 * @param workflowName - The workflow name for display
 * @param options - Optional configuration
 * @returns The run ID if successful, null otherwise
 */
export async function createTestRun(
  projectId: string,
  workflowId: string,
  workflowName: string,
  options?: StartTestRunOptions,
): Promise<CreateTestRunResponse | null> {
  if (!projectId) {
    console.warn("[TestRunCreation] No project ID provided, skipping test run creation");
    return null;
  }

  try {
    console.log(`[TestRunCreation] Starting test run for workflow: ${workflowName}`);

    const input: CreateTestRunInput = {
      project_id: projectId,
      workflow_id: workflowId,
      workflow_name: workflowName,
      total_states: options?.totalStates ?? 0,
      total_transitions: options?.totalTransitions ?? 0,
      description: options?.description,
    };

    const response = await invoke<CreateTestRunResponse>("create_test_run", { input });

    console.log(`[TestRunCreation] Test run created: ${response.run_id}`);
    return response;
  } catch (error) {
    console.error("[TestRunCreation] Failed to create test run:", error);
    return null;
  }
}

/**
 * Parse a workflow ID string to a number or null
 */
export function parseWorkflowId(workflowId: string): number | null {
  const parsed = parseInt(workflowId, 10);
  return isNaN(parsed) ? null : parsed;
}

/**
 * Create initial active run state from a creation response
 */
export function createActiveRunState(
  response: CreateTestRunResponse,
  projectId: string,
  workflowId: string,
): ActiveRunState {
  return {
    runId: response.run_id,
    projectId: projectId,
    workflowId: parseWorkflowId(workflowId),
  };
}

/**
 * Create an empty/inactive run state
 */
export function createEmptyRunState(): ActiveRunState {
  return {
    runId: null,
    projectId: null,
    workflowId: null,
  };
}
