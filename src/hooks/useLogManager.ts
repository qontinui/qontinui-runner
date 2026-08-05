/**
 * useLogManager Hook
 *
 * React hook for interacting with the LogManager singleton.
 * Provides reactive state for logs and image recognition logs.
 *
 * @example
 * ```tsx
 * function LogViewer() {
 *   const { logs, imageLogs, addLog, clearGeneralLogs } = useLogManager();
 *
 *   return (
 *     <div>
 *       {logs.map(log => (
 *         <div key={log.id}>{log.message}</div>
 *       ))}
 *     </div>
 *   );
 * }
 * ```
 */

import { useCallback, useSyncExternalStore } from "react";
import { logManager, LogEntry, ImageRecognitionEntry, AiOutputEntry } from "../managers";
import type { AiOutputLine, ActionLogEntry } from "../managers/logging";
import type { LogSourceContent } from "../types/projectLogs";

export interface UseLogManagerResult {
  /** General log entries */
  logs: LogEntry[];

  /** Image recognition log entries */
  imageLogs: ImageRecognitionEntry[];

  /** AI output log entries */
  aiOutputLogs: AiOutputEntry[];

  /** Add a general log entry */
  addLog: (level: LogEntry["level"], message: string) => void;

  /** Add an image recognition log entry */
  addImageLog: (entry: ImageRecognitionEntry) => void;

  /** Add an AI output log entry */
  addAiOutputLog: (
    line: string,
    source: string,
    actionId?: string,
    taskRunId?: string,
    sessionId?: string,
    sessionName?: string,
    phase?: string,
    phaseIteration?: number,
  ) => void;

  /** Get filtered logs by level */
  getFilteredLogs: (level: string) => LogEntry[];

  /** Clear general logs */
  clearGeneralLogs: () => void;

  /** Clear image logs */
  clearImageLogs: () => void;

  /** Clear AI output logs */
  clearAiOutputLogs: () => void;

  /** Clear all logs */
  clearAllLogs: () => void;

  /** Copy logs to clipboard */
  copyLogs: (
    type: "general" | "image" | "actions" | "ai" | "external",
    data?: {
      actionLogs?: ActionLogEntry[];
      aiOutputLines?: AiOutputLine[];
      externalSources?: LogSourceContent[];
    },
  ) => Promise<boolean>;

  /** Get log count */
  logCount: number;

  /** Get image log count */
  imageLogCount: number;

  /** Get AI output log count */
  aiOutputLogCount: number;
}

/** Reactive slice of {@link UseLogManagerResult}. */
export type UseLogDataResult = Pick<
  UseLogManagerResult,
  "logs" | "imageLogs" | "aiOutputLogs" | "logCount" | "imageLogCount" | "aiOutputLogCount"
>;

/** Non-reactive slice: every entry is an identity-stable callback. */
export type UseLogActionsResult = Omit<
  UseLogManagerResult,
  keyof UseLogDataResult
>;

// Module-level so the `useSyncExternalStore` subscribe argument is stable.
const subscribeToLogs = (onStoreChange: () => void) => logManager.subscribe(onStoreChange);
const getGeneralLogs = () => logManager.getGeneralLogs();
const getImageLogs = () => logManager.getImageLogs();
const getAiOutputLogs = () => logManager.getAiOutputLogs();

/**
 * Subscribe to log CONTENT.
 *
 * Call this only in the panels that actually render logs. It used to be
 * fused into `useLogManager()` at the App root, so a single AI output line
 * re-rendered the entire app — including the always-mounted (CSS-hidden)
 * terminal tree (plan `2026-07-28-runner-many-sessions-performance` §0 A2).
 *
 * Safe as a `useSyncExternalStore` source because `LogStore`'s getters now
 * return cached arrays whose identity is stable between mutations.
 */
export function useLogData(): UseLogDataResult {
  const logs = useSyncExternalStore(subscribeToLogs, getGeneralLogs);
  const imageLogs = useSyncExternalStore(subscribeToLogs, getImageLogs);
  const aiOutputLogs = useSyncExternalStore(subscribeToLogs, getAiOutputLogs);
  return {
    logs,
    imageLogs,
    aiOutputLogs,
    logCount: logs.length,
    imageLogCount: imageLogs.length,
    aiOutputLogCount: aiOutputLogs.length,
  };
}

/**
 * Log MUTATION surface, with no subscription at all — safe to call from a
 * component that must not re-render when a log line lands (the App root).
 */
export function useLogActions(): UseLogActionsResult {
  // Wrap manager methods
  const addLog = useCallback((level: LogEntry["level"], message: string) => {
    logManager.addLog(level, message);
  }, []);

  const addImageLog = useCallback((entry: ImageRecognitionEntry) => {
    logManager.addImageLog(entry);
  }, []);

  const addAiOutputLog = useCallback(
    (
      line: string,
      source: string,
      actionId?: string,
      taskRunId?: string,
      sessionId?: string,
      sessionName?: string,
      phase?: string,
      phaseIteration?: number,
    ) => {
      logManager.addAiOutputLog(
        line,
        source,
        actionId,
        taskRunId,
        sessionId,
        sessionName,
        phase,
        phaseIteration,
      );
    },
    [],
  );

  const getFilteredLogs = useCallback((level: string) => {
    return logManager.getFilteredLogs(level);
  }, []);

  const clearGeneralLogs = useCallback(() => {
    logManager.clearGeneralLogs();
  }, []);

  const clearImageLogs = useCallback(() => {
    logManager.clearImageLogs();
  }, []);

  const clearAiOutputLogs = useCallback(() => {
    logManager.clearAiOutputLogs();
  }, []);

  const clearAllLogs = useCallback(() => {
    logManager.clearAllLogs();
  }, []);

  const copyLogs = useCallback(
    async (
      type: "general" | "image" | "actions" | "ai" | "external",
      data?: {
        actionLogs?: ActionLogEntry[];
        aiOutputLines?: AiOutputLine[];
        externalSources?: LogSourceContent[];
      },
    ) => {
      return await logManager.copyLogs(type, data);
    },
    [],
  );

  return {
    addLog,
    addImageLog,
    addAiOutputLog,
    getFilteredLogs,
    clearGeneralLogs,
    clearImageLogs,
    clearAiOutputLogs,
    clearAllLogs,
    copyLogs,
  };
}

/**
 * Actions + reactive data in one call. Convenience for panels that need
 * both; anything on a hot render path should reach for `useLogActions()` or
 * `useLogData()` directly so it only pays for what it uses.
 */
export function useLogManager(): UseLogManagerResult {
  return { ...useLogData(), ...useLogActions() };
}
