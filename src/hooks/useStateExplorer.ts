/**
 * useStateExplorer Hook
 *
 * Manages state and operations for the State Explorer.
 * Provides methods to run exploration, preview plans, view history, etc.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ExplorationConfig,
  ExplorationResult,
  ExplorationHistoryItem,
  ExplorationPlan,
  ExplorationStrategy,
} from "../types/state-explorer";

interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

export interface UseStateExplorerReturn {
  /** Available exploration strategies */
  strategies: ExplorationStrategy[];
  /** Whether strategies are loading */
  strategiesLoading: boolean;

  /** Exploration history items */
  history: ExplorationHistoryItem[];
  /** Whether history is loading */
  historyLoading: boolean;
  /** Refresh history */
  refreshHistory: () => Promise<void>;

  /** Currently selected report for viewing */
  selectedReport: ExplorationResult | null;
  /** Whether report is loading */
  reportLoading: boolean;
  /** Load a specific report */
  loadReport: (runId: string) => Promise<void>;
  /** Clear selected report */
  clearReport: () => void;

  /** Preview exploration plan */
  previewPlan: (config: ExplorationConfig) => Promise<ExplorationPlan | null>;
  /** Whether preview is loading */
  previewLoading: boolean;
  /** Current preview plan */
  currentPlan: ExplorationPlan | null;

  /** Start an exploration task */
  startExploration: (config: ExplorationConfig) => Promise<ExplorationResult | null>;
  /** Whether exploration is running */
  explorationRunning: boolean;

  /** Get AI analysis prompt for a report */
  getAnalysisPrompt: (runId: string) => Promise<string | null>;

  /** Clear exploration history */
  clearHistory: (olderThanDays?: number) => Promise<number>;

  /** Current error message */
  error: string | null;
  /** Clear error */
  clearError: () => void;
}

export function useStateExplorer(): UseStateExplorerReturn {
  const [strategies, setStrategies] = useState<ExplorationStrategy[]>([]);
  const [strategiesLoading, setStrategiesLoading] = useState(false);

  const [history, setHistory] = useState<ExplorationHistoryItem[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);

  const [selectedReport, setSelectedReport] = useState<ExplorationResult | null>(null);
  const [reportLoading, setReportLoading] = useState(false);

  const [currentPlan, setCurrentPlan] = useState<ExplorationPlan | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);

  const [explorationRunning, setExplorationRunning] = useState(false);

  const [error, setError] = useState<string | null>(null);

  const clearError = useCallback(() => setError(null), []);

  // Load strategies on mount
  useEffect(() => {
    const loadStrategies = async () => {
      setStrategiesLoading(true);
      try {
        const response = await invoke<CommandResponse<{ strategies: ExplorationStrategy[] }>>(
          "get_exploration_strategies",
        );
        if (response.success && response.data?.strategies) {
          setStrategies(response.data.strategies);
        }
      } catch (err) {
        console.error("[useStateExplorer] Failed to load strategies:", err);
        setError(`Failed to load strategies: ${err}`);
      } finally {
        setStrategiesLoading(false);
      }
    };

    loadStrategies();
  }, []);

  // Refresh history
  const refreshHistory = useCallback(async () => {
    setHistoryLoading(true);
    try {
      const response = await invoke<CommandResponse<{ runs: ExplorationHistoryItem[] }>>(
        "get_exploration_history",
        { limit: 50 },
      );
      if (response.success && response.data?.runs) {
        setHistory(response.data.runs);
      }
    } catch (err) {
      console.error("[useStateExplorer] Failed to load history:", err);
      setError(`Failed to load history: ${err}`);
    } finally {
      setHistoryLoading(false);
    }
  }, []);

  // Load history on mount. Deferred into a microtask so the setState
  // inside refreshHistory doesn't fire synchronously from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void refreshHistory();
    });
    return () => {
      cancelled = true;
    };
  }, [refreshHistory]);

  // Load a specific report
  const loadReport = useCallback(async (runId: string) => {
    setReportLoading(true);
    setError(null);
    try {
      const response = await invoke<CommandResponse<ExplorationResult>>("get_exploration_report", {
        runId,
      });
      if (response.success && response.data) {
        // The data is the report itself, not wrapped
        setSelectedReport(response.data);
      } else {
        throw new Error(response.message || "Failed to load report");
      }
    } catch (err) {
      console.error("[useStateExplorer] Failed to load report:", err);
      setError(`Failed to load report: ${err}`);
      setSelectedReport(null);
    } finally {
      setReportLoading(false);
    }
  }, []);

  const clearReport = useCallback(() => {
    setSelectedReport(null);
  }, []);

  // Preview exploration plan
  const previewPlan = useCallback(
    async (config: ExplorationConfig): Promise<ExplorationPlan | null> => {
      setPreviewLoading(true);
      setError(null);
      try {
        const response = await invoke<CommandResponse<ExplorationPlan>>(
          "preview_exploration_plan",
          { config },
        );
        if (response.success && response.data) {
          setCurrentPlan(response.data);
          return response.data;
        } else {
          throw new Error(response.message || "Failed to preview plan");
        }
      } catch (err) {
        console.error("[useStateExplorer] Failed to preview plan:", err);
        setError(`Failed to preview plan: ${err}`);
        setCurrentPlan(null);
        return null;
      } finally {
        setPreviewLoading(false);
      }
    },
    [],
  );

  // Start exploration task
  const startExploration = useCallback(
    async (config: ExplorationConfig): Promise<ExplorationResult | null> => {
      setExplorationRunning(true);
      setError(null);
      try {
        const response = await invoke<CommandResponse<ExplorationResult>>("start_exploration", {
          config,
        });
        if (response.success && response.data) {
          // Refresh history after completion
          await refreshHistory();
          return response.data;
        } else {
          throw new Error(response.message || "Exploration task failed");
        }
      } catch (err) {
        console.error("[useStateExplorer] Exploration failed:", err);
        setError(`Exploration failed: ${err}`);
        return null;
      } finally {
        setExplorationRunning(false);
      }
    },
    [refreshHistory],
  );

  // Get AI analysis prompt
  const getAnalysisPrompt = useCallback(async (runId: string): Promise<string | null> => {
    try {
      const response = await invoke<CommandResponse<{ run_id: string; prompt: string }>>(
        "get_exploration_analysis_prompt",
        { runId },
      );
      if (response.success && response.data?.prompt) {
        return response.data.prompt;
      }
      return null;
    } catch (err) {
      console.error("[useStateExplorer] Failed to get analysis prompt:", err);
      setError(`Failed to get analysis prompt: ${err}`);
      return null;
    }
  }, []);

  // Clear history
  const clearHistory = useCallback(
    async (olderThanDays?: number): Promise<number> => {
      try {
        const response = await invoke<CommandResponse<{ removed: number }>>(
          "clear_exploration_history",
          { olderThanDays },
        );
        if (response.success && response.data) {
          await refreshHistory();
          return response.data.removed;
        }
        return 0;
      } catch (err) {
        console.error("[useStateExplorer] Failed to clear history:", err);
        setError(`Failed to clear history: ${err}`);
        return 0;
      }
    },
    [refreshHistory],
  );

  return {
    strategies,
    strategiesLoading,
    history,
    historyLoading,
    refreshHistory,
    selectedReport,
    reportLoading,
    loadReport,
    clearReport,
    previewPlan,
    previewLoading,
    currentPlan,
    startExploration,
    explorationRunning,
    getAnalysisPrompt,
    clearHistory,
    error,
    clearError,
  };
}
