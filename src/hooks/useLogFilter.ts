/**
 * useLogFilter Hook
 *
 * Manages log filtering by level.
 * Provides filtered logs based on selected log level.
 */

import { useState, useMemo } from "react";
import type { LogEntry } from "../managers/LogManager";

export type LogLevel = "all" | "info" | "warning" | "error" | "debug" | "success";

export interface UseLogFilterResult {
  logLevel: LogLevel;
  setLogLevel: (level: LogLevel) => void;
  filteredLogs: LogEntry[];
}

/**
 * Hook for filtering logs by level
 */
export function useLogFilter(logs: LogEntry[]): UseLogFilterResult {
  const [logLevel, setLogLevel] = useState<LogLevel>("all");

  const filteredLogs = useMemo(() => {
    if (logLevel === "all") {
      return logs;
    }
    return logs.filter((log) => log.level === logLevel);
  }, [logs, logLevel]);

  return {
    logLevel,
    setLogLevel,
    filteredLogs,
  };
}
