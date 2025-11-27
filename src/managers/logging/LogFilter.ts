/**
 * LogFilter
 *
 * Responsible for filtering log entries based on various criteria.
 * Follows Single Responsibility Principle - handles only filtering logic.
 */

import type { LogEntry } from "./LogStore";

/**
 * Filters log entries by level.
 */
export class LogFilter {
  /**
   * Filter logs by level
   * @param logs - Array of log entries to filter
   * @param level - Level to filter by, or "all" for no filtering
   * @returns Filtered log entries
   */
  filterByLevel(logs: LogEntry[], level: string): LogEntry[] {
    if (level === "all") {
      return logs;
    }
    return logs.filter((log) => log.level === level);
  }
}
