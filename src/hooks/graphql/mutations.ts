/**
 * GraphQL mutation hooks.
 *
 * These provide typed React hooks for mutating data via GraphQL.
 * Each mutation hook returns [executeFn, result] following Apollo's convention.
 * Cache updates are handled automatically where possible.
 */

import { useMutation } from "@apollo/client/react";

import {
  STOP_TASK_RUN_MUTATION,
  PAUSE_TASK_RUN_MUTATION,
  UNPAUSE_TASK_RUN_MUTATION,
  DELETE_TASK_RUN_MUTATION,
  CREATE_TASK_RUN_MUTATION,
  RUN_WORKFLOW_MUTATION,
  UPDATE_FINDING_STATUS_MUTATION,
  RESPOND_TO_FINDING_MUTATION,
  DELETE_WORKFLOW_MUTATION,
  TASK_RUNS_QUERY,
  WORKFLOWS_QUERY,
} from "./documents";

import type { TaskRunData } from "./queries";

// ==========================================================================
// Task Run Mutations
// ==========================================================================

/**
 * Stop a running task run.
 * Refetches task run list after completion.
 */
export function useStopTaskRunGql() {
  return useMutation<{ stopTaskRun: boolean }, { id: string }>(
    STOP_TASK_RUN_MUTATION,
    {
      refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
    },
  );
}

/**
 * Pause a running task run.
 */
export function usePauseTaskRunGql() {
  return useMutation<{ pauseTaskRun: boolean }, { id: string }>(
    PAUSE_TASK_RUN_MUTATION,
    {
      refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
    },
  );
}

/**
 * Unpause a paused task run.
 */
export function useUnpauseTaskRunGql() {
  return useMutation<{ unpauseTaskRun: boolean }, { id: string }>(
    UNPAUSE_TASK_RUN_MUTATION,
    {
      refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
    },
  );
}

/**
 * Delete a task run.
 * Removes from cache after completion.
 */
export function useDeleteTaskRunGql() {
  return useMutation<{ deleteTaskRun: boolean }, { id: string }>(
    DELETE_TASK_RUN_MUTATION,
    {
      refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
      update(cache, _result, { variables }) {
        if (variables?.id) {
          const cacheId = cache.identify({ __typename: "GqlTaskRun", id: variables.id });
          if (cacheId) {
            cache.evict({ id: cacheId });
            cache.gc();
          }
        }
      },
    },
  );
}

/**
 * Create a new task run.
 */
export function useCreateTaskRunGql() {
  return useMutation<
    { createTaskRun: TaskRunData },
    {
      input: {
        taskName: string;
        prompt?: string;
        taskType?: string;
        configId?: string;
        workflowName?: string;
        workflowId?: string;
        maxSessions?: number;
        autoContinue?: boolean;
      };
    }
  >(CREATE_TASK_RUN_MUTATION, {
    refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
  });
}

/**
 * Run a workflow by ID. Returns the created task run.
 */
export function useRunWorkflowGql() {
  return useMutation<
    { runWorkflow: TaskRunData },
    { workflowId: string; prompt?: string }
  >(RUN_WORKFLOW_MUTATION, {
    refetchQueries: [{ query: TASK_RUNS_QUERY, variables: { limit: 50 } }],
  });
}

// ==========================================================================
// Finding Mutations
// ==========================================================================

/**
 * Update a finding's status.
 */
export function useUpdateFindingStatusGql() {
  return useMutation<
    { updateFindingStatus: boolean },
    {
      input: {
        findingId: string;
        status: string;
        resolution?: string;
      };
    }
  >(UPDATE_FINDING_STATUS_MUTATION);
}

/**
 * Respond to a finding that needs user input.
 */
export function useRespondToFindingGql() {
  return useMutation<
    { respondToFinding: boolean },
    { findingId: string; response: string }
  >(RESPOND_TO_FINDING_MUTATION);
}

// ==========================================================================
// Workflow Mutations
// ==========================================================================

/**
 * Delete a workflow.
 */
export function useDeleteWorkflowGql() {
  return useMutation<{ deleteWorkflow: boolean }, { id: string }>(
    DELETE_WORKFLOW_MUTATION,
    {
      refetchQueries: [{ query: WORKFLOWS_QUERY }],
      update(cache, _result, { variables }) {
        if (variables?.id) {
          const cacheId = cache.identify({ __typename: "GqlWorkflowSummary", id: variables.id });
          if (cacheId) {
            cache.evict({ id: cacheId });
            cache.gc();
          }
        }
      },
    },
  );
}
