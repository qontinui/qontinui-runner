/**
 * LogManager
 *
 * High-level coordinator for the logging system. Delegates responsibilities
 * to specialized modules following the Single Responsibility Principle.
 *
 * Responsibilities:
 * - Coordinate between specialized logging modules
 * - Provide a unified API for the application
 * - Handle deduplication of consecutive log messages
 */

import { invoke } from "@tauri-apps/api/core";
import { LogStore, LogFilter, LogFormatter, TimestampFormatter, ImageLogHandler } from "./logging";
import type {
  LogEntry,
  ImageRecognitionEntry,
  ActionLogEntry,
  AiOutputEntry,
  AiOutputLine,
} from "./logging";
import type { LogSourceContent } from "../types/projectLogs";

// Re-export types for backward compatibility
export type { LogEntry, ImageRecognitionEntry, AiOutputEntry };

type LogListener = () => void;

/**
 * Singleton manager that coordinates the logging system.
 * Delegates specific concerns to specialized modules.
 */
class LogManager {
  private logStore: LogStore;
  private logFilter: LogFilter;
  private logFormatter: LogFormatter;
  private timestampFormatter: TimestampFormatter;
  private imageLogHandler: ImageLogHandler;

  // Deduplication state
  private lastLogMessage: string | null = null;

  // Initialization state
  private initialized = false;
  private initializationPromise: Promise<void> | null = null;

  constructor() {
    this.logStore = new LogStore();
    this.logFilter = new LogFilter();
    this.logFormatter = new LogFormatter();
    this.timestampFormatter = new TimestampFormatter();
    this.imageLogHandler = new ImageLogHandler(this.timestampFormatter);
  }

  /**
   * Initialize the LogManager by loading conversation history from disk.
   * This should be called once on app startup.
   * Safe to call multiple times - subsequent calls will return immediately.
   */
  async initialize(): Promise<void> {
    // Return existing promise if initialization is in progress
    if (this.initializationPromise) {
      return this.initializationPromise;
    }

    // Skip if already initialized
    if (this.initialized) {
      return;
    }

    this.initializationPromise = this.doInitialize();
    return this.initializationPromise;
  }

  private async doInitialize(): Promise<void> {
    try {
      console.log("[LogManager] Initializing - loading conversation history...");

      // Rust returns snake_case field names
      interface RustAiOutputEntry {
        id: string;
        timestamp: number;
        line: string;
        source: string;
        action_id?: string;
      }

      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: { entries: RustAiOutputEntry[] };
      }>("load_ai_output_log");

      if (result.success && result.data?.entries) {
        // Map from Rust snake_case to TypeScript camelCase
        const entries: AiOutputEntry[] = result.data.entries.map((entry) => ({
          id: entry.id,
          timestamp: entry.timestamp,
          line: entry.line,
          source: entry.source,
          actionId: entry.action_id,
        }));

        this.logStore.loadAiOutputLogs(entries);
        console.log(`[LogManager] Loaded ${entries.length} conversation entries from history`);
      } else {
        console.log("[LogManager] No conversation history to load");
      }

      this.initialized = true;
    } catch (error) {
      console.error("[LogManager] Failed to load conversation history:", error);
      // Mark as initialized even on error to prevent repeated failures
      this.initialized = true;
    }
  }

  /**
   * Check if the LogManager has been initialized
   */
  isInitialized(): boolean {
    return this.initialized;
  }

  /**
   * Add a general log entry
   */
  addLog(level: LogEntry["level"], message: string): void {
    // Check for duplicate consecutive messages
    if (message === this.lastLogMessage) {
      return;
    }
    this.lastLogMessage = message;

    const logEntry: LogEntry = {
      id: `log-${Date.now()}-${Math.random()}`,
      timestamp: this.timestampFormatter.formatCurrentTime(),
      level,
      message,
    };

    this.logStore.addGeneralLog(logEntry);
  }

  /**
   * Add an image recognition log entry
   */
  addImageLog(entry: ImageRecognitionEntry): void {
    this.logStore.addImageLog(entry);
  }

  /**
   * Process raw image recognition data and add as log entry
   * This is a convenience method for EventHandlers
   */
  processImageRecognitionData(data: any): void {
    const entry = this.imageLogHandler.processImageRecognitionData(data);
    this.addImageLog(entry);
  }

  /**
   * Get all general logs
   */
  getGeneralLogs(): LogEntry[] {
    return this.logStore.getGeneralLogs();
  }

  /**
   * Get filtered general logs by level
   */
  getFilteredLogs(level: string): LogEntry[] {
    const allLogs = this.logStore.getGeneralLogs();
    return this.logFilter.filterByLevel(allLogs, level);
  }

  /**
   * Get all image logs
   */
  getImageLogs(): ImageRecognitionEntry[] {
    return this.logStore.getImageLogs();
  }

  /**
   * Add an AI output log entry
   */
  addAiOutputLog(line: string, source: string, actionId?: string): void {
    const entry: AiOutputEntry = {
      id: `ai-${Date.now()}-${Math.random()}`,
      timestamp: Date.now(),
      line,
      source,
      actionId,
    };
    this.logStore.addAiOutputLog(entry);

    // Persist to file for QA feedback loop
    invoke("append_ai_output_log", {
      entry: {
        id: entry.id,
        timestamp: entry.timestamp,
        line: entry.line,
        source: entry.source,
        action_id: entry.actionId,
      },
    }).catch((err) => {
      console.warn("[LogManager] Failed to persist AI output log:", err);
    });
  }

  /**
   * Get all AI output logs
   */
  getAiOutputLogs(): AiOutputEntry[] {
    return this.logStore.getAiOutputLogs();
  }

  /**
   * Clear general logs
   */
  clearGeneralLogs(): void {
    this.logStore.clearGeneralLogs();
    this.lastLogMessage = null;
  }

  /**
   * Clear image logs
   */
  clearImageLogs(): void {
    this.logStore.clearImageLogs();
  }

  /**
   * Clear AI output logs
   */
  clearAiOutputLogs(): void {
    this.logStore.clearAiOutputLogs();
  }

  /**
   * Clear all logs
   */
  clearAllLogs(): void {
    this.clearGeneralLogs();
    this.clearImageLogs();
    this.clearAiOutputLogs();
  }

  /**
   * Copy logs to clipboard
   * @param type - Type of logs to copy
   * @param data - Optional data for certain log types:
   *   - actionLogs: Required when type is "actions"
   *   - aiOutputLines: Required when type is "ai"
   *   - externalSources: Required when type is "external"
   */
  async copyLogs(
    type: "general" | "image" | "actions" | "ai" | "external",
    data?: {
      actionLogs?: ActionLogEntry[];
      aiOutputLines?: AiOutputLine[];
      externalSources?: LogSourceContent[];
    },
  ): Promise<boolean> {
    let text = "";

    if (type === "general") {
      const logs = this.logStore.getGeneralLogs();
      text = this.logFormatter.formatGeneralLogs(logs);
    } else if (type === "image") {
      const logs = this.logStore.getImageLogs();
      text = this.logFormatter.formatImageLogs(logs);
    } else if (type === "actions" && data?.actionLogs) {
      text = this.logFormatter.formatActionLogs(data.actionLogs);
    } else if (type === "ai" && data?.aiOutputLines) {
      text = this.logFormatter.formatAiOutputLogs(data.aiOutputLines);
    } else if (type === "external" && data?.externalSources) {
      text = this.logFormatter.formatExternalLogs(data.externalSources);
    }

    return await this.logFormatter.copyToClipboard(text);
  }

  /**
   * Subscribe to log changes
   * @returns Unsubscribe function
   */
  subscribe(listener: LogListener): () => void {
    return this.logStore.subscribe(listener);
  }

  /**
   * Get log count
   */
  getLogCount(): number {
    return this.logStore.getGeneralLogCount();
  }

  /**
   * Get image log count
   */
  getImageLogCount(): number {
    return this.logStore.getImageLogCount();
  }

  /**
   * Get AI output log count
   */
  getAiOutputLogCount(): number {
    return this.logStore.getAiOutputLogCount();
  }
}

// Export singleton instance
export const logManager = new LogManager();
