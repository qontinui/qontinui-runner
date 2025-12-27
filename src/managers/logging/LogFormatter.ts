/**
 * LogFormatter
 *
 * Responsible for formatting log entries for display and clipboard operations.
 * Follows Single Responsibility Principle - handles only formatting concerns.
 */

import type { LogEntry, ImageRecognitionEntry } from "./LogStore";
import type { LogSourceContent } from "../../types/projectLogs";

export interface ActionLogEntry {
  action_type: string;
  timestamp: number;
  duration?: number | null;
  status: string;
  error?: string | null;
  metadata?: Record<string, unknown> | null;
}

export interface AiOutputLine {
  id: string;
  timestamp: number;
  line: string;
  source: string;
  actionId?: string;
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
   * Format AI output logs as text for clipboard
   */
  formatAiOutputLogs(lines: AiOutputLine[]): string {
    return lines
      .map((entry) => {
        const timestamp = new Date(entry.timestamp).toLocaleTimeString("en-US", {
          hour12: false,
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        });
        const sourceLabel = entry.source === "prompt" ? "USER" : "AI";
        return `[${timestamp}] [${sourceLabel}] ${entry.line}`;
      })
      .join("\n");
  }

  /**
   * Format external/project logs as text for clipboard
   */
  formatExternalLogs(sources: LogSourceContent[]): string {
    if (sources.length === 0) return "";

    return sources
      .map((source) => {
        const header = `=== ${source.sourceName} (${source.filePath}) ===`;
        const content = source.lines.join("\n");
        return `${header}\n${content}`;
      })
      .join("\n\n");
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
