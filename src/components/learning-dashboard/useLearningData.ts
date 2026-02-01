/**
 * useLearningData.ts
 *
 * React hooks for the Learning Insights Dashboard.
 * Provides data fetching and state management for learning analytics.
 * Includes real-time updates via Tauri events.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { learningService } from "../../services/learning-service";
import { useLearningUpdates, useTaskStatusUpdates } from "../../hooks/useRealtimeUpdates";
import type {
  LearningDashboardData,
  Pattern,
  Insight,
  Feedback,
  TaskRunWithOutcome,
  CurrentRunningTask,
  LearningStatsSummary,
} from "../../types/learning";

interface UseLearningDataResult {
  data: LearningDashboardData | null;
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
  /** Whether realtime updates are connected */
  isRealtimeConnected: boolean;
  /** Whether a task is currently running */
  isTaskRunning: boolean;
}

/**
 * Hook to fetch all learning dashboard data with real-time updates
 */
export function useLearningData(): UseLearningDataResult {
  const [data, setData] = useState<LearningDashboardData | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await learningService.getLearningDashboardData();
      setData(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch learning data");
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Subscribe to real-time learning updates
  const { isConnected: isRealtimeConnected } = useLearningUpdates({
    onUpdate: () => {
      // Refetch data when a new learning outcome is recorded
      fetchData();
    },
  });

  // Subscribe to task status updates
  const { isTaskRunning } = useTaskStatusUpdates();

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  return { data, isLoading, error, refetch: fetchData, isRealtimeConnected, isTaskRunning };
}

interface UseLearningPatternsResult {
  patterns: Pattern[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch learning patterns
 */
export function useLearningPatterns(): UseLearningPatternsResult {
  const [patterns, setPatterns] = useState<Pattern[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchPatterns = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await learningService.getLearningPatterns();
      setPatterns(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch patterns");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchPatterns();
  }, [fetchPatterns]);

  return { patterns, isLoading, error, refetch: fetchPatterns };
}

interface UseLearningInsightsResult {
  insights: Insight[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch learning insights
 */
export function useLearningInsights(): UseLearningInsightsResult {
  const [insights, setInsights] = useState<Insight[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchInsights = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await learningService.getLearningInsights();
      setInsights(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch insights");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchInsights();
  }, [fetchInsights]);

  return { insights, isLoading, error, refetch: fetchInsights };
}

interface UseBestStrategyResult {
  bestStrategy: [string, number] | null;
  isLoading: boolean;
}

/**
 * Hook to get the best performing strategy
 */
export function useBestStrategy(): UseBestStrategyResult {
  const [bestStrategy, setBestStrategy] = useState<[string, number] | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    learningService
      .getBestStrategy()
      .then(setBestStrategy)
      .catch(() => setBestStrategy(null))
      .finally(() => setIsLoading(false));
  }, []);

  return { bestStrategy, isLoading };
}

interface UseFeedbackResult {
  feedback: Feedback[];
  isLoading: boolean;
  error: string | null;
}

/**
 * Hook to get feedback for a specific context
 */
export function useFeedbackForContext(context: string): UseFeedbackResult {
  const [feedback, setFeedback] = useState<Feedback[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!context) {
      setFeedback([]);
      setIsLoading(false);
      return;
    }

    setIsLoading(true);
    setError(null);
    learningService
      .getFeedbackForContext(context)
      .then(setFeedback)
      .catch((err) => {
        setError(err instanceof Error ? err.message : "Failed to fetch feedback");
      })
      .finally(() => setIsLoading(false));
  }, [context]);

  return { feedback, isLoading, error };
}

/**
 * Actions for learning data management
 */
export const learningActions = {
  async addSampleData(): Promise<void> {
    await learningService.addSampleLearningData();
  },

  async clearData(): Promise<void> {
    await learningService.clearLearningData();
  },

  async exportData(): Promise<string> {
    return learningService.exportLearningData();
  },

  async importData(json: string): Promise<void> {
    return learningService.importLearningData(json);
  },
};

// ============================================================================
// Recent Tasks with Learning Outcomes
// ============================================================================

interface UseRecentTasksResult {
  tasks: TaskRunWithOutcome[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
  /** Whether realtime updates are connected */
  isRealtimeConnected: boolean;
}

/**
 * Hook to fetch recent tasks with their learning outcomes.
 * Automatically refreshes when new learning outcomes are recorded.
 */
export function useRecentTasks(limit?: number): UseRecentTasksResult {
  const [tasks, setTasks] = useState<TaskRunWithOutcome[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchTasks = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await learningService.getRecentTasksWithOutcomes(limit);
      setTasks(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch recent tasks");
    } finally {
      setIsLoading(false);
    }
  }, [limit]);

  // Subscribe to real-time learning updates
  const { isConnected: isRealtimeConnected } = useLearningUpdates({
    onUpdate: () => {
      // Refetch tasks when a new learning outcome is recorded
      fetchTasks();
    },
  });

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  return { tasks, isLoading, error, refetch: fetchTasks, isRealtimeConnected };
}

interface UseCurrentTaskResult {
  currentTask: CurrentRunningTask | null;
  isLoading: boolean;
  refetch: () => Promise<void>;
  /** Whether realtime updates are connected */
  isRealtimeConnected: boolean;
  /** Current status from real-time events */
  realtimeStatus: string | null;
  /** Current iteration from real-time events */
  realtimeIteration: number | null;
}

/**
 * Hook to get the current running task.
 * Uses real-time updates for immediate status changes,
 * with fallback polling for resilience.
 */
export function useCurrentTask(): UseCurrentTaskResult {
  const [currentTask, setCurrentTask] = useState<CurrentRunningTask | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const pollIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchCurrentTask = useCallback(async () => {
    setIsLoading(true);
    try {
      const result = await learningService.getCurrentRunningTask();
      setCurrentTask(result);
    } catch {
      setCurrentTask(null);
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Subscribe to real-time task status updates
  const { isConnected: isRealtimeConnected, currentTask: realtimeTask } = useTaskStatusUpdates({
    onStatusChange: (payload) => {
      // Refetch on task start/complete to get full task details
      if (
        payload.status === "started" ||
        payload.status === "completed" ||
        payload.status === "failed" ||
        payload.status === "stopped"
      ) {
        fetchCurrentTask();
      }
    },
  });

  useEffect(() => {
    fetchCurrentTask();

    // Use longer polling interval when realtime is connected
    const pollInterval = isRealtimeConnected ? 30000 : 5000;
    pollIntervalRef.current = setInterval(fetchCurrentTask, pollInterval);

    return () => {
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current);
      }
    };
  }, [fetchCurrentTask, isRealtimeConnected]);

  return {
    currentTask,
    isLoading,
    refetch: fetchCurrentTask,
    isRealtimeConnected,
    realtimeStatus: realtimeTask?.status ?? null,
    realtimeIteration: realtimeTask?.iteration ?? null,
  };
}

interface UseLearningStatsResult {
  stats: LearningStatsSummary | null;
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch learning statistics summary
 */
export function useLearningStats(): UseLearningStatsResult {
  const [stats, setStats] = useState<LearningStatsSummary | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStats = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await learningService.getLearningStatsSummary();
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
