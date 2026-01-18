/**
 * useCheckpointData.ts
 *
 * React hooks for the Checkpoint Browser (Time-Travel Debugging).
 * Provides data fetching and state management for checkpoints.
 * Includes real-time updates via Tauri events.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { checkpointService } from "../../services/checkpoint-service";
import { useCheckpointUpdates, useTaskStatusUpdates } from "../../hooks/useRealtimeUpdates";
import type {
  Checkpoint,
  CheckpointSummary,
  CheckpointDiff,
  CheckpointStats,
  ReplaySession,
  LineageTree,
  ReplayFromCheckpointResponse,
} from "../../types/checkpoint";
import type { CheckpointCreatedPayload } from "../../types/realtimeEvents";

interface UseCheckpointListResult {
  checkpoints: CheckpointSummary[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
  /** Whether realtime updates are connected */
  isRealtimeConnected: boolean;
  /** The most recently created checkpoint (via realtime event) */
  lastCreatedCheckpoint: CheckpointCreatedPayload | null;
  /** Currently active checkpoint ID (the one being created/just created) */
  activeCheckpointId: string | null;
}

/**
 * Hook to fetch all checkpoints, optionally filtered by task ID.
 * Automatically refreshes when new checkpoints are created via realtime events.
 */
export function useCheckpointList(taskId?: string): UseCheckpointListResult {
  const [checkpoints, setCheckpoints] = useState<CheckpointSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeCheckpointId, setActiveCheckpointId] = useState<string | null>(null);

  const fetchCheckpoints = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await checkpointService.listCheckpoints(taskId);
      setCheckpoints(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch checkpoints");
    } finally {
      setIsLoading(false);
    }
  }, [taskId]);

  // Subscribe to real-time checkpoint created events
  const { isConnected: isRealtimeConnected, lastCheckpoint: lastCreatedCheckpoint } =
    useCheckpointUpdates({
      taskId,
      onCheckpointCreated: (payload) => {
        // Set the active checkpoint ID for highlighting
        setActiveCheckpointId(payload.checkpoint_id);

        // Clear the highlight after 3 seconds
        setTimeout(() => {
          setActiveCheckpointId((current) => (current === payload.checkpoint_id ? null : current));
        }, 3000);

        // Refetch to get the new checkpoint
        fetchCheckpoints();
      },
    });

  useEffect(() => {
    fetchCheckpoints();
  }, [fetchCheckpoints]);

  return {
    checkpoints,
    isLoading,
    error,
    refetch: fetchCheckpoints,
    isRealtimeConnected,
    lastCreatedCheckpoint,
    activeCheckpointId,
  };
}

interface UseCheckpointResult {
  checkpoint: Checkpoint | null;
  isLoading: boolean;
  error: string | null;
}

/**
 * Hook to fetch a single checkpoint by ID
 */
export function useCheckpoint(id: string | null): UseCheckpointResult {
  const [checkpoint, setCheckpoint] = useState<Checkpoint | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) {
      setCheckpoint(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    checkpointService
      .getCheckpoint(id)
      .then(setCheckpoint)
      .catch((err) => {
        setError(err instanceof Error ? err.message : "Failed to fetch checkpoint");
      })
      .finally(() => setIsLoading(false));
  }, [id]);

  return { checkpoint, isLoading, error };
}

interface UseCheckpointStatsResult {
  stats: CheckpointStats | null;
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch checkpoint statistics
 */
export function useCheckpointStats(): UseCheckpointStatsResult {
  const [stats, setStats] = useState<CheckpointStats | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStats = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await checkpointService.getCheckpointStats();
      setStats(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch stats");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchStats();
  }, [fetchStats]);

  return { stats, isLoading, error, refetch: fetchStats };
}

interface UseTaskIdsResult {
  taskIds: string[];
  isLoading: boolean;
}

/**
 * Hook to fetch unique task IDs with checkpoints
 */
export function useCheckpointTaskIds(): UseTaskIdsResult {
  const [taskIds, setTaskIds] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    checkpointService
      .getCheckpointTaskIds()
      .then(setTaskIds)
      .catch(() => setTaskIds([]))
      .finally(() => setIsLoading(false));
  }, []);

  return { taskIds, isLoading };
}

interface UseMostRecentTaskResult {
  mostRecentTaskId: string | null;
  isLoading: boolean;
}

/**
 * Hook to get the most recent task ID that has checkpoints.
 * Used for auto-selecting in the checkpoint browser.
 */
export function useMostRecentTaskWithCheckpoints(): UseMostRecentTaskResult {
  const [mostRecentTaskId, setMostRecentTaskId] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    checkpointService
      .getMostRecentTaskWithCheckpoints()
      .then(setMostRecentTaskId)
      .catch(() => setMostRecentTaskId(null))
      .finally(() => setIsLoading(false));
  }, []);

  return { mostRecentTaskId, isLoading };
}

interface UseAutoSelectTaskResult {
  selectedTaskId: string | undefined;
  setSelectedTaskId: (taskId: string | undefined) => void;
  isAutoSelected: boolean;
}

/**
 * Hook that automatically selects the most recent task with checkpoints
 * unless the user has already made a selection.
 */
export function useAutoSelectTask(taskIds: string[]): UseAutoSelectTaskResult {
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>(undefined);
  const [isAutoSelected, setIsAutoSelected] = useState(false);
  const { mostRecentTaskId, isLoading: mostRecentLoading } = useMostRecentTaskWithCheckpoints();
  const userHasSelected = useRef(false);

  // Auto-select the most recent task if user hasn't made a selection
  useEffect(() => {
    if (
      !mostRecentLoading &&
      !userHasSelected.current &&
      mostRecentTaskId &&
      taskIds.includes(mostRecentTaskId)
    ) {
      setSelectedTaskId(mostRecentTaskId);
      setIsAutoSelected(true);
    }
  }, [mostRecentTaskId, mostRecentLoading, taskIds]);

  const handleSetSelectedTaskId = useCallback((taskId: string | undefined) => {
    userHasSelected.current = true;
    setIsAutoSelected(false);
    setSelectedTaskId(taskId);
  }, []);

  return { selectedTaskId, setSelectedTaskId: handleSetSelectedTaskId, isAutoSelected };
}

interface UseCheckpointDiffResult {
  diff: CheckpointDiff | null;
  isLoading: boolean;
  error: string | null;
}

/**
 * Hook to compare two checkpoints
 */
export function useCheckpointDiff(
  fromId: string | null,
  toId: string | null,
): UseCheckpointDiffResult {
  const [diff, setDiff] = useState<CheckpointDiff | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!fromId || !toId) {
      setDiff(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    checkpointService
      .compareCheckpoints(fromId, toId)
      .then(setDiff)
      .catch((err) => {
        setError(err instanceof Error ? err.message : "Failed to compare checkpoints");
      })
      .finally(() => setIsLoading(false));
  }, [fromId, toId]);

  return { diff, isLoading, error };
}

/**
 * Hook to fetch lineage tree for a task
 */
export function useLineageTree(taskRunId: string | null) {
  const [lineage, setLineage] = useState<LineageTree | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!taskRunId) {
      setLineage(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    checkpointService
      .getReplayLineage(taskRunId)
      .then(setLineage)
      .catch((err) => {
        setError(err instanceof Error ? err.message : "Failed to fetch lineage");
      })
      .finally(() => setIsLoading(false));
  }, [taskRunId]);

  return { lineage, isLoading, error };
}

/**
 * Actions for checkpoint management
 */
export const checkpointActions = {
  async createManualCheckpoint(
    taskId: string,
    state: string,
    iteration: number,
    name?: string,
    description?: string,
  ): Promise<string> {
    return checkpointService.createCheckpoint(taskId, state, iteration, name, description);
  },

  async deleteCheckpoint(id: string): Promise<boolean> {
    return checkpointService.deleteCheckpoint(id);
  },

  /**
   * Start a replay from a checkpoint (legacy method).
   * @deprecated Use replayFromCheckpoint for full functionality.
   */
  async startReplay(checkpointId: string): Promise<ReplaySession> {
    return checkpointService.startReplaySession(checkpointId);
  },

  /**
   * Start a full replay from a checkpoint.
   * Creates a new task run branched from the checkpoint's state.
   */
  async replayFromCheckpoint(checkpointId: string): Promise<ReplayFromCheckpointResponse> {
    return checkpointService.replayFromCheckpoint(checkpointId);
  },

  /**
   * Get lineage tree for a task.
   */
  async getLineage(taskRunId: string): Promise<LineageTree | null> {
    return checkpointService.getReplayLineage(taskRunId);
  },

  async addSampleCheckpoints(): Promise<void> {
    return checkpointService.addSampleCheckpoints();
  },

  async clearAllCheckpoints(): Promise<void> {
    return checkpointService.clearAllCheckpoints();
  },
};
