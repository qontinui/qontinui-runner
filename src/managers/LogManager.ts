/**
 * LogManager
 *
 * Manages general logs and image recognition logs with filtering, deduplication,
 * and clipboard operations. Follows single responsibility principle.
 */

export interface LogEntry {
  id: string;
  timestamp: string;
  level: "info" | "warning" | "error" | "debug" | "success";
  message: string;
}

export interface ImageRecognitionEntry {
  id: string;
  timestamp: string;
  screenshotTimestamp?: string;
  node: string;
  template: string;
  confidence: number;
  found: boolean;
  threshold: number;
  location?: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
  gap?: number;
  percentOff?: number;
  bestMatchLocation?: string;
  screenshotPath?: string;
  screenshotData?: string; // Base64 encoded screenshot (when no file path)
  visualDebugImage?: string; // Base64 encoded annotated screenshot with colored match boxes
  templatePath?: string;
  imageData?: string; // Base64 encoded template image
  debug?: any; // Debug data with top_matches for displaying match details
}

type LogListener = () => void;

/**
 * Singleton manager for application logs
 */
class LogManager {
  private generalLogs: LogEntry[] = [];
  private imageLogs: ImageRecognitionEntry[] = [];
  private listeners = new Set<LogListener>();
  private lastLogMessage: string | null = null;

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
      timestamp: new Date().toLocaleTimeString("en-US", {
        hour12: false,
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }),
      level,
      message,
    };

    this.generalLogs.push(logEntry);
    this.notifyListeners();
  }

  /**
   * Add an image recognition log entry
   */
  addImageLog(entry: ImageRecognitionEntry): void {
    this.imageLogs.push(entry);
    this.notifyListeners();
  }

  /**
   * Get all general logs
   */
  getGeneralLogs(): LogEntry[] {
    return [...this.generalLogs];
  }

  /**
   * Get filtered general logs by level
   */
  getFilteredLogs(level: string): LogEntry[] {
    if (level === "all") {
      return this.getGeneralLogs();
    }
    return this.generalLogs.filter((log) => log.level === level);
  }

  /**
   * Get all image logs
   */
  getImageLogs(): ImageRecognitionEntry[] {
    return [...this.imageLogs];
  }

  /**
   * Clear general logs
   */
  clearGeneralLogs(): void {
    this.generalLogs = [];
    this.lastLogMessage = null;
    this.notifyListeners();
  }

  /**
   * Clear image logs
   */
  clearImageLogs(): void {
    this.imageLogs = [];
    this.notifyListeners();
  }

  /**
   * Clear all logs
   */
  clearAllLogs(): void {
    this.clearGeneralLogs();
    this.clearImageLogs();
  }

  /**
   * Copy logs to clipboard
   * @param type - Type of logs to copy
   * @param actionLogs - Optional action logs data (required when type is "actions")
   */
  async copyLogs(
    type: "general" | "image" | "actions",
    actionLogs?: Array<{
      action_type: string;
      timestamp: number;
      duration?: number | null;
      status: string;
      error?: string | null;
      metadata?: any;
    }>
  ): Promise<boolean> {
    try {
      let text = "";

      if (type === "general") {
        text = this.generalLogs
          .map((log) => `[${log.timestamp}] [${log.level.toUpperCase()}] ${log.message}`)
          .join("\n");
      } else if (type === "image") {
        text = this.imageLogs
          .map(
            (entry) =>
              `[${entry.timestamp}] ${entry.node} - ${entry.template} - ` +
              `${entry.found ? "FOUND" : "NOT FOUND"} (${(entry.confidence * 100).toFixed(1)}%)`
          )
          .join("\n");
      } else if (type === "actions" && actionLogs) {
        text = actionLogs
          .map((action) => {
            const timestamp = new Date(action.timestamp * 1000).toLocaleTimeString("en-US", {
              hour12: false,
              hour: "2-digit",
              minute: "2-digit",
              second: "2-digit",
            });
            const duration = action.duration
              ? ` (${(action.duration * 1000).toFixed(0)}ms)`
              : "";
            const status = action.status.toUpperCase();
            const error = action.error ? ` - ERROR: ${action.error}` : "";

            return `[${timestamp}] ${action.action_type}${duration} - ${status}${error}`;
          })
          .join("\n");
      }

      if (text) {
        await navigator.clipboard.writeText(text);
        return true;
      }
      return false;
    } catch (error) {
      console.error("Failed to copy logs:", error);
      return false;
    }
  }

  /**
   * Subscribe to log changes
   * @returns Unsubscribe function
   */
  subscribe(listener: LogListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /**
   * Notify all listeners of changes
   */
  private notifyListeners(): void {
    this.listeners.forEach((listener) => listener());
  }

  /**
   * Get log count
   */
  getLogCount(): number {
    return this.generalLogs.length;
  }

  /**
   * Get image log count
   */
  getImageLogCount(): number {
    return this.imageLogs.length;
  }
}

// Export singleton instance
export const logManager = new LogManager();
