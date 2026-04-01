/**
 * useTaskRunControls Hook
 *
 * Provides typed, cache-aware task run lifecycle controls via GraphQL mutations.
 * Use this instead of raw tracedFetch calls for stop/pause/unpause/delete.
 *
 * Each action returns a promise that resolves to success/failure.
 * On success, the Apollo cache is automatically updated via refetchQueries.
 *
 * Usage:
 *   const { stop, pause, unpause, remove } = useTaskRunControls();
 *   await stop("task-123"); // returns true on success
 */

import { useCallback } from "react";
import {
  useStopTaskRunGql,
  usePauseTaskRunGql,
  useUnpauseTaskRunGql,
  useDeleteTaskRunGql,
  useUpdateFindingStatusGql,
  useRespondToFindingGql,
} from "./mutations";

export interface TaskRunControls {
  /** Stop a running task (kills AI processes). */
  stop: (id: string) => Promise<boolean>;
  /** Pause a running task. */
  pause: (id: string) => Promise<boolean>;
  /** Unpause a paused task. */
  unpause: (id: string) => Promise<boolean>;
  /** Delete a task run. */
  remove: (id: string) => Promise<boolean>;
  /** Whether any mutation is currently in flight. */
  loading: boolean;
}

export function useTaskRunControls(): TaskRunControls {
  const [stopMutation, { loading: stopLoading }] = useStopTaskRunGql();
  const [pauseMutation, { loading: pauseLoading }] = usePauseTaskRunGql();
  const [unpauseMutation, { loading: unpauseLoading }] = useUnpauseTaskRunGql();
  const [deleteMutation, { loading: deleteLoading }] = useDeleteTaskRunGql();

  const stop = useCallback(
    async (id: string): Promise<boolean> => {
      try {
        const { data } = await stopMutation({ variables: { id } });
        return data?.stopTaskRun ?? false;
      } catch (e) {
        console.error("[useTaskRunControls] Stop failed:", e);
        return false;
      }
    },
    [stopMutation],
  );

  const pause = useCallback(
    async (id: string): Promise<boolean> => {
      try {
        const { data } = await pauseMutation({ variables: { id } });
        return data?.pauseTaskRun ?? false;
      } catch (e) {
        console.error("[useTaskRunControls] Pause failed:", e);
        return false;
      }
    },
    [pauseMutation],
  );

  const unpause = useCallback(
    async (id: string): Promise<boolean> => {
      try {
        const { data } = await unpauseMutation({ variables: { id } });
        return data?.unpauseTaskRun ?? false;
      } catch (e) {
        console.error("[useTaskRunControls] Unpause failed:", e);
        return false;
      }
    },
    [unpauseMutation],
  );

  const remove = useCallback(
    async (id: string): Promise<boolean> => {
      try {
        const { data } = await deleteMutation({ variables: { id } });
        return data?.deleteTaskRun ?? false;
      } catch (e) {
        console.error("[useTaskRunControls] Delete failed:", e);
        return false;
      }
    },
    [deleteMutation],
  );

  return {
    stop,
    pause,
    unpause,
    remove,
    loading: stopLoading || pauseLoading || unpauseLoading || deleteLoading,
  };
}

export interface FindingControls {
  /** Update a finding's status (resolve, defer, won't fix). */
  updateStatus: (findingId: string, status: string, resolution?: string) => Promise<boolean>;
  /** Respond to a finding that needs user input. */
  respond: (findingId: string, response: string) => Promise<boolean>;
  loading: boolean;
}

export function useFindingControls(): FindingControls {
  const [updateMutation, { loading: updateLoading }] = useUpdateFindingStatusGql();
  const [respondMutation, { loading: respondLoading }] = useRespondToFindingGql();

  const updateStatus = useCallback(
    async (findingId: string, status: string, resolution?: string): Promise<boolean> => {
      try {
        const { data } = await updateMutation({
          variables: { input: { findingId, status, resolution } },
        });
        return data?.updateFindingStatus ?? false;
      } catch (e) {
        console.error("[useFindingControls] Update failed:", e);
        return false;
      }
    },
    [updateMutation],
  );

  const respond = useCallback(
    async (findingId: string, response: string): Promise<boolean> => {
      try {
        const { data } = await respondMutation({
          variables: { findingId, response },
        });
        return data?.respondToFinding ?? false;
      } catch (e) {
        console.error("[useFindingControls] Respond failed:", e);
        return false;
      }
    },
    [respondMutation],
  );

  return {
    updateStatus,
    respond,
    loading: updateLoading || respondLoading,
  };
}
