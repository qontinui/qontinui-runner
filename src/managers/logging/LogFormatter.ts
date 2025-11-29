/**
 * LogFormatter
 *
 * Responsible for formatting log entries for display and clipboard operations.
 * Follows Single Responsibility Principle - handles only formatting concerns.
 */

import type { LogEntry, ImageRecognitionEntry } from "./LogStore";

export interface ActionLogEntry {
  action_type: string;
  timestamp: number;
  duration?: number | null;
  status: string;
  error?: string | null;
  metadata?: any;
}

/**
 * Formats log entries for various output formats.
 */
export class LogFormatter {
  /**
   * Format general logs as text for clipboard
   */
  formatGeneralLogs(logs: LogEntry[]): string {
    return logs
      .map((log) => `[${log.timestamp}] [${log.level.toUpperCase()}] ${log.message}`)
      .join("\n");
  }

  /**
   * Format image recognition logs as text for clipboard
   */
  formatImageLogs(logs: ImageRecognitionEntry[]): string {
    return logs
      .map(
        (entry) =>
          `[${entry.timestamp}] ${entry.node} - ${entry.template} - ` +
          `${entry.found ? "FOUND" : "NOT FOUND"} (${(entry.confidence * 100).toFixed(1)}%)`,
      )
      .join("\n");
  }

  /**
   * Format action logs as text for clipboard
   */
  formatActionLogs(logs: ActionLogEntry[]): string {
    return logs
      .map((action) => {
        const timestamp = new Date(action.timestamp * 1000).toLocaleTimeString("en-US", {
          hour12: false,
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        });
        const duration = action.duration ? ` (${(action.duration * 1000).toFixed(0)}ms)` : "";
        const status = action.status.toUpperCase();
        const error = action.error ? ` - ERROR: ${action.error}` : "";

        return `[${timestamp}] ${action.action_type}${duration} - ${status}${error}`;
      })
      .join("\n");
  }

  /**
   * Copy formatted text to clipboard
   */
  async copyToClipboard(text: string): Promise<boolean> {
    try {
      if (text) {
        await navigator.clipboard.writeText(text);
        return true;
      }
      return false;
    } catch (error) {
      console.error("Failed to copy to clipboard:", error);
      return false;
    }
  }
}
