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
import { logManager, LogEntry, ImageRecognitionEntry } from "../managers";

export interface UseLogManagerResult {
  /** General log entries */
  logs: LogEntry[];

  /** Image recognition log entries */
  imageLogs: ImageRecognitionEntry[];

  /** Add a general log entry */
  addLog: (level: LogEntry["level"], message: string) => void;

  /** Add an image recognition log entry */
  addImageLog: (entry: ImageRecognitionEntry) => void;

  /** Get filtered logs by level */
  getFilteredLogs: (level: string) => LogEntry[];

  /** Clear general logs */
  clearGeneralLogs: () => void;

  /** Clear image logs */
  clearImageLogs: () => void;

  /** Clear all logs */
  clearAllLogs: () => void;

  /** Copy logs to clipboard */
  copyLogs: (type: "general" | "image" | "actions", actionLogs?: any[]) => Promise<boolean>;

  /** Get log count */
  logCount: number;

  /** Get image log count */
  imageLogCount: number;
}

/**
 * Hook for accessing and managing application logs
 */
export function useLogManager(): UseLogManagerResult {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [imageLogs, setImageLogs] = useState<ImageRecognitionEntry[]>([]);

  // Subscribe to log changes
  useEffect(() => {
    // Initial fetch
    setLogs(logManager.getGeneralLogs());
    setImageLogs(logManager.getImageLogs());

    // Subscribe to changes
    const unsubscribe = logManager.subscribe(() => {
      setLogs(logManager.getGeneralLogs());
      setImageLogs(logManager.getImageLogs());
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

  const getFilteredLogs = useCallback((level: string) => {
    return logManager.getFilteredLogs(level);
  }, []);

  const clearGeneralLogs = useCallback(() => {
    logManager.clearGeneralLogs();
  }, []);

  const clearImageLogs = useCallback(() => {
    logManager.clearImageLogs();
  }, []);

  const clearAllLogs = useCallback(() => {
    logManager.clearAllLogs();
  }, []);

  const copyLogs = useCallback(
    async (type: "general" | "image" | "actions", actionLogs?: any[]) => {
      return await logManager.copyLogs(type, actionLogs);
    },
    [],
  );

  return {
    logs,
    imageLogs,
    addLog,
    addImageLog,
    getFilteredLogs,
    clearGeneralLogs,
    clearImageLogs,
    clearAllLogs,
    copyLogs,
    logCount: logManager.getLogCount(),
    imageLogCount: logManager.getImageLogCount(),
  };
}
