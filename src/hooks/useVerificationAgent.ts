/**
 * useVerificationAgent Hook
 *
 * Manages state and operations for the AI Verification Agent.
 * Provides methods to run verification, preview plans, view history, etc.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  VerificationTaskConfig,
  VerificationResult,
  VerificationHistoryItem,
  VerificationPlan,
  VerificationStrategy,
} from "../types/verification-agent";

interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

export interface UseVerificationAgentReturn {
  /** Available verification strategies */
  strategies: VerificationStrategy[];
  /** Whether strategies are loading */
  strategiesLoading: boolean;

  /** Verification history items */
  history: VerificationHistoryItem[];
  /** Whether history is loading */
  historyLoading: boolean;
  /** Refresh history */
  refreshHistory: () => Promise<void>;

  /** Currently selected report for viewing */
  selectedReport: VerificationResult | null;
  /** Whether report is loading */
  reportLoading: boolean;
  /** Load a specific report */
  loadReport: (runId: string) => Promise<void>;
  /** Clear selected report */
  clearReport: () => void;

  /** Preview verification plan */
  previewPlan: (config: VerificationTaskConfig) => Promise<VerificationPlan | null>;
  /** Whether preview is loading */
  previewLoading: boolean;
  /** Current preview plan */
  currentPlan: VerificationPlan | null;

  /** Start a verification task */
  startVerification: (config: VerificationTaskConfig) => Promise<VerificationResult | null>;
  /** Whether verification is running */
  verificationRunning: boolean;

  /** Get AI analysis prompt for a report */
  getAnalysisPrompt: (runId: string) => Promise<string | null>;

  /** Clear verification history */
  clearHistory: (olderThanDays?: number) => Promise<number>;

  /** Current error message */
  error: string | null;
  /** Clear error */
  clearError: () => void;
}

export function useVerificationAgent(): UseVerificationAgentReturn {
  const [strategies, setStrategies] = useState<VerificationStrategy[]>([]);
  const [strategiesLoading, setStrategiesLoading] = useState(false);

  const [history, setHistory] = useState<VerificationHistoryItem[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);

  const [selectedReport, setSelectedReport] = useState<VerificationResult | null>(null);
  const [reportLoading, setReportLoading] = useState(false);

  const [currentPlan, setCurrentPlan] = useState<VerificationPlan | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);

  const [verificationRunning, setVerificationRunning] = useState(false);

  const [error, setError] = useState<string | null>(null);

  const clearError = useCallback(() => setError(null), []);

  // Load strategies on mount
  useEffect(() => {
    const loadStrategies = async () => {
      setStrategiesLoading(true);
      try {
        const response = await invoke<CommandResponse<{ strategies: VerificationStrategy[] }>>(
          "get_verification_strategies",
        );
        if (response.success && response.data?.strategies) {
          setStrategies(response.data.strategies);
        }
      } catch (err) {
        console.error("[useVerificationAgent] Failed to load strategies:", err);
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
      const response = await invoke<CommandResponse<{ runs: VerificationHistoryItem[] }>>(
        "get_verification_history",
        { limit: 50 },
      );
      if (response.success && response.data?.runs) {
        setHistory(response.data.runs);
      }
    } catch (err) {
      console.error("[useVerificationAgent] Failed to load history:", err);
      setError(`Failed to load history: ${err}`);
    } finally {
      setHistoryLoading(false);
    }
  }, []);

  // Load history on mount
  useEffect(() => {
    refreshHistory();
  }, [refreshHistory]);

  // Load a specific report
  const loadReport = useCallback(async (runId: string) => {
    setReportLoading(true);
    setError(null);
    try {
      const response = await invoke<CommandResponse<VerificationResult>>(
        "get_verification_report",
        { runId },
      );
      if (response.success && response.data) {
        // The data is the report itself, not wrapped
        setSelectedReport(response.data);
      } else {
        throw new Error(response.message || "Failed to load report");
      }
    } catch (err) {
      console.error("[useVerificationAgent] Failed to load report:", err);
      setError(`Failed to load report: ${err}`);
      setSelectedReport(null);
    } finally {
      setReportLoading(false);
    }
  }, []);

  const clearReport = useCallback(() => {
    setSelectedReport(null);
  }, []);

  // Preview verification plan
  const previewPlan = useCallback(
    async (config: VerificationTaskConfig): Promise<VerificationPlan | null> => {
      setPreviewLoading(true);
      setError(null);
      try {
        const response = await invoke<CommandResponse<VerificationPlan>>(
          "preview_verification_plan",
          { config },
        );
        if (response.success && response.data) {
          setCurrentPlan(response.data);
          return response.data;
        } else {
          throw new Error(response.message || "Failed to preview plan");
        }
      } catch (err) {
        console.error("[useVerificationAgent] Failed to preview plan:", err);
        setError(`Failed to preview plan: ${err}`);
        setCurrentPlan(null);
        return null;
      } finally {
        setPreviewLoading(false);
      }
    },
    [],
  );

  // Start verification task
  const startVerification = useCallback(
    async (config: VerificationTaskConfig): Promise<VerificationResult | null> => {
      setVerificationRunning(true);
      setError(null);
      try {
        const response = await invoke<CommandResponse<VerificationResult>>(
          "start_verification_task",
          { config },
        );
        if (response.success && response.data) {
          // Refresh history after completion
          await refreshHistory();
          return response.data;
        } else {
          throw new Error(response.message || "Verification task failed");
        }
      } catch (err) {
        console.error("[useVerificationAgent] Verification failed:", err);
        setError(`Verification failed: ${err}`);
        return null;
      } finally {
        setVerificationRunning(false);
      }
    },
    [refreshHistory],
  );

  // Get AI analysis prompt
  const getAnalysisPrompt = useCallback(async (runId: string): Promise<string | null> => {
    try {
      const response = await invoke<CommandResponse<{ run_id: string; prompt: string }>>(
        "get_verification_analysis_prompt",
        { runId },
      );
      if (response.success && response.data?.prompt) {
        return response.data.prompt;
      }
      return null;
    } catch (err) {
      console.error("[useVerificationAgent] Failed to get analysis prompt:", err);
      setError(`Failed to get analysis prompt: ${err}`);
      return null;
    }
  }, []);

  // Clear history
  const clearHistory = useCallback(
    async (olderThanDays?: number): Promise<number> => {
      try {
        const response = await invoke<CommandResponse<{ removed: number }>>(
          "clear_verification_history",
          { olderThanDays },
        );
        if (response.success && response.data) {
          await refreshHistory();
          return response.data.removed;
        }
        return 0;
      } catch (err) {
        console.error("[useVerificationAgent] Failed to clear history:", err);
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
    startVerification,
    verificationRunning,
    getAnalysisPrompt,
    clearHistory,
    error,
    clearError,
  };
}
