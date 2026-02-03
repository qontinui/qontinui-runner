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

import { useState, useEffect, useCallback } from "react";
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

/**
 * Hook for accessing and managing application logs
 */
export function useLogManager(): UseLogManagerResult {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [imageLogs, setImageLogs] = useState<ImageRecognitionEntry[]>([]);
  const [aiOutputLogs, setAiOutputLogs] = useState<AiOutputEntry[]>([]);

  // Subscribe to log changes
  useEffect(() => {
    // Initial fetch
    setLogs(logManager.getGeneralLogs());
    setImageLogs(logManager.getImageLogs());
    setAiOutputLogs(logManager.getAiOutputLogs());

    // Subscribe to changes
    const unsubscribe = logManager.subscribe(() => {
      setLogs(logManager.getGeneralLogs());
      setImageLogs(logManager.getImageLogs());
      setAiOutputLogs(logManager.getAiOutputLogs());
    });

    return unsubscribe;
  }, []);

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
    logs,
    imageLogs,
    aiOutputLogs,
    addLog,
    addImageLog,
    addAiOutputLog,
    getFilteredLogs,
    clearGeneralLogs,
    clearImageLogs,
    clearAiOutputLogs,
    clearAllLogs,
    copyLogs,
    logCount: logManager.getLogCount(),
    imageLogCount: logManager.getImageLogCount(),
    aiOutputLogCount: logManager.getAiOutputLogCount(),
  };
}
