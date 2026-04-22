/**
 * ActiveRunsContext
 *
 * React context for tracking multiple concurrent workflow runs.
 * Enables multi-run dashboard view with run selection and GUI lock awareness.
 */

import { createContext, useContext, useState, useEffect, useCallback, type ReactNode } from "react";
import type { TaskActivityInfo } from "../types/dashboard/widget-registry";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

/**
 * Bridge mode type.
 */
export type BridgeMode = "gui" | "headless" | null;

/**
 * Information about a single active run.
 */
export interface ActiveRun {
  /** Unique run ID */
  runId: string;
  /** Task name */
  taskName: string | null;
  /** Task type */
  taskType: "task" | "automation" | "scheduled" | null;
  /** Current status */
  status: "pending" | "running" | "completed" | "failed" | "stopped";
  /** Whether this run has GUI lock */
  hasGuiLock: boolean;
  /** Associated bridge ID (if using multi-bridge) */
  bridgeId: string | null;
  /** Bridge mode: gui or headless */
  mode: BridgeMode;
  /** Run start time */
  startTime: number | null;
  /** Workflow name */
  workflowName: string | null;
  /** Current iteration */
  iteration: number;
  /** Max iterations. `null` indicates no cap (unlimited). */
  maxIterations: number | null;
  /** Full task activity info for dashboard use */
  taskInfo: TaskActivityInfo;
}

/**
 * GUI lock information.
 */
export interface GuiLockInfo {
  /** ID of the bridge holding the lock */
  holderId: string | null;
  /** Timestamp when lock was acquired */
  acquiredAt: number | null;
}

/**
 * Bridge count summary.
 */
export interface BridgeCountSummary {
  /** Number of GUI mode bridges */
  gui: number;
  /** Number of headless mode bridges */
  headless: number;
  /** Total active bridges */
  total: number;
}

/**
 * Active runs context value.
 */
interface ActiveRunsContextValue {
  /** All active runs (running or recently completed) */
  activeRuns: ActiveRun[];
  /** ID of the currently selected/displayed run */
  selectedRunId: string | null;
  /** GUI lock holder bridge ID */
  guiLockHolderId: string | null;
  /** Whether there are multiple active runs */
  hasMultipleRuns: boolean;
  /** Bridge count summary */
  bridgeCounts: BridgeCountSummary;
  /** Select a run to display in the main view */
  selectRun: (runId: string) => void;
  /** Get the currently selected run */
  selectedRun: ActiveRun | null;
  /** Whether loading initial data */
  isLoading: boolean;
  /** Refresh runs data */
  refresh: () => Promise<void>;
}

const ActiveRunsContext = createContext<ActiveRunsContextValue | null>(null);

/**
 * Running task data from the API.
 */
interface RunningTaskData {
  id: string;
  task_name: string;
  task_type: "task" | "automation" | "scheduled";
  status: string;
  config_id?: string;
  workflow_name?: string;
  prompt?: string;
  execution_steps_json?: string;
  current_iteration?: number;
  max_iterations?: number;
  created_at: string;
  bridge_id?: string;
}

/**
 * Bridge data from the API.
 */
interface BridgeData {
  bridge_id: string;
  run_id: string | null;
  mode: "gui" | "headless";
  monitor_indices: number[];
  state: string;
  has_gui_lock: boolean;
  created_at: number;
}

/**
 * Convert task API data to ActiveRun.
 */
function taskToActiveRun(task: RunningTaskData, hasGuiLock: boolean, mode: BridgeMode): ActiveRun {
  const taskInfo: TaskActivityInfo = {
    taskId: task.id,
    taskType: task.task_type,
    taskName: task.task_name,
    hasConfig: !!task.config_id,
    hasPrompt: !!task.prompt,
    hasPlaywrightScripts: false, // Would need to parse execution_steps_json
    hasVerificationTests: false,
    workflowName: task.workflow_name ?? null,
    iteration: task.current_iteration ?? 1,
    // null = unlimited; preserve so display can show "∞" rather than "1".
    maxIterations: task.max_iterations ?? null,
    activities: [],
    startTime: task.created_at ? new Date(task.created_at).getTime() : null,
  };

  return {
    runId: task.id,
    taskName: task.task_name,
    taskType: task.task_type,
    status: task.status as ActiveRun["status"],
    hasGuiLock,
    bridgeId: task.bridge_id ?? null,
    mode,
    startTime: taskInfo.startTime,
    workflowName: task.workflow_name ?? null,
    iteration: task.current_iteration ?? 1,
    // null = unlimited; preserve so display can show "∞" rather than "1".
    maxIterations: task.max_iterations ?? null,
    taskInfo,
  };
}

/**
 * Provider props.
 */
interface ActiveRunsProviderProps {
  children: ReactNode;
}

/**
 * Active runs context provider.
 */
export function ActiveRunsProvider({ children }: ActiveRunsProviderProps) {
  const [activeRuns, setActiveRuns] = useState<ActiveRun[]>([]);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [guiLockHolderId, setGuiLockHolderId] = useState<string | null>(null);
  const [bridgeCounts, setBridgeCounts] = useState<BridgeCountSummary>({
    gui: 0,
    headless: 0,
    total: 0,
  });
  const [isLoading, setIsLoading] = useState(true);

  // Track previously seen run IDs to detect new runs
  const [previousRunIds, setPreviousRunIds] = useState<Set<string>>(new Set());

  /**
   * Fetch active runs and GUI lock info.
   */
  const refresh = useCallback(async () => {
    try {
      // Fetch running tasks
      const tasksResponse = await tracedFetch(`${getApiBase()}/task-runs/running`);
      let tasks: RunningTaskData[] = [];
      if (tasksResponse.ok) {
        tasks = await tasksResponse.json();
      }

      // Fetch bridges info
      let bridges: BridgeData[] = [];
      try {
        const bridgesResponse = await tracedFetch(`${getApiBase()}/bridges`);
        if (bridgesResponse.ok) {
          const bridgesData = await bridgesResponse.json();
          if (bridgesData.success && Array.isArray(bridgesData.data)) {
            bridges = bridgesData.data;
          }
        }
      } catch {
        // Bridges endpoint may not be available yet
      }

      // Build bridge lookup map by bridge_id
      const bridgeByBridgeId = new Map<string, BridgeData>();
      // Also build a map by run_id for tasks that might have run_id but no bridge_id
      const bridgeByRunId = new Map<string, BridgeData>();
      for (const bridge of bridges) {
        bridgeByBridgeId.set(bridge.bridge_id, bridge);
        if (bridge.run_id) {
          bridgeByRunId.set(bridge.run_id, bridge);
        }
      }

      // Calculate bridge counts
      const guiCount = bridges.filter((b) => b.mode === "gui").length;
      const headlessCount = bridges.filter((b) => b.mode === "headless").length;
      setBridgeCounts({
        gui: guiCount,
        headless: headlessCount,
        total: bridges.length,
      });

      // Fetch GUI lock info
      let guiLockHolder: string | null = null;
      try {
        const lockResponse = await tracedFetch(`${getApiBase()}/gui-lock`);
        if (lockResponse.ok) {
          const lockData = await lockResponse.json();
          if (lockData.success && lockData.data?.holder_id) {
            guiLockHolder = lockData.data.holder_id;
          }
        }
      } catch {
        // GUI lock endpoint may not be available yet
      }

      setGuiLockHolderId(guiLockHolder);

      // Convert tasks to active runs
      const runs = tasks.map((task) => {
        // Find bridge info for this task
        let bridge: BridgeData | undefined;
        if (task.bridge_id) {
          bridge = bridgeByBridgeId.get(task.bridge_id);
        }
        // Fallback: try to find by run_id
        if (!bridge) {
          bridge = bridgeByRunId.get(task.id);
        }

        const hasGuiLock = bridge ? bridge.has_gui_lock : false;
        const mode: BridgeMode = bridge ? bridge.mode : null;
        return taskToActiveRun(task, hasGuiLock, mode);
      });

      // Detect new runs that weren't in the previous set
      const currentRunIds = new Set(runs.map((r) => r.runId));
      const newRuns = runs.filter((r) => !previousRunIds.has(r.runId));

      setActiveRuns(runs);
      setPreviousRunIds(currentRunIds);

      // Auto-select logic:
      // 1. If no run is selected, select the newest running task
      // 2. If a new run appears, optionally auto-select it (if current selection is completed)
      // 3. If selected run is no longer in the list, select the first available

      if (!selectedRunId && runs.length > 0) {
        // Prefer running tasks over completed ones
        const runningTask = runs.find((r) => r.status === "running");
        setSelectedRunId(runningTask?.runId ?? runs[0].runId);
      } else if (selectedRunId) {
        const currentSelection = runs.find((r) => r.runId === selectedRunId);

        if (!currentSelection) {
          // Selected run is no longer in the list
          const runningTask = runs.find((r) => r.status === "running");
          setSelectedRunId(runningTask?.runId ?? (runs.length > 0 ? runs[0].runId : null));
        } else if (
          currentSelection.status !== "running" &&
          newRuns.length > 0 &&
          newRuns.some((r) => r.status === "running")
        ) {
          // Current selection finished, and there's a new running task - switch to it
          const newRunning = newRuns.find((r) => r.status === "running");
          if (newRunning) {
            setSelectedRunId(newRunning.runId);
          }
        }
      }
    } catch {
      // API may not be available
    } finally {
      setIsLoading(false);
    }
  }, [selectedRunId, previousRunIds]);

  // Initial fetch and polling
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [refresh]);

  /**
   * Select a run to display.
   */
  const selectRun = useCallback((runId: string) => {
    setSelectedRunId(runId);
  }, []);

  /**
   * Get the currently selected run.
   */
  const selectedRun = activeRuns.find((r) => r.runId === selectedRunId) ?? null;

  const value: ActiveRunsContextValue = {
    activeRuns,
    selectedRunId,
    guiLockHolderId,
    hasMultipleRuns: activeRuns.length > 1,
    bridgeCounts,
    selectRun,
    selectedRun,
    isLoading,
    refresh,
  };

  return <ActiveRunsContext.Provider value={value}>{children}</ActiveRunsContext.Provider>;
}

/**
 * Hook to access active runs context.
 * Throws if used outside of ActiveRunsProvider.
 */
export function useActiveRuns(): ActiveRunsContextValue {
  const context = useContext(ActiveRunsContext);
  if (!context) {
    throw new Error("useActiveRuns must be used within an ActiveRunsProvider");
  }
  return context;
}

/**
 * Hook to access active runs context optionally.
 * Returns null if used outside of ActiveRunsProvider.
 */
export function useActiveRunsOptional(): ActiveRunsContextValue | null {
  return useContext(ActiveRunsContext);
}

/**
 * Hook to get the currently selected run's task info.
 * Useful for components that need to filter data by run.
 */
export function useSelectedRunTaskInfo(): TaskActivityInfo | null {
  const context = useContext(ActiveRunsContext);
  return context?.selectedRun?.taskInfo ?? null;
}

/**
 * Hook to check if current run has GUI lock.
 */
export function useSelectedRunHasGuiLock(): boolean {
  const context = useContext(ActiveRunsContext);
  return context?.selectedRun?.hasGuiLock ?? false;
}
