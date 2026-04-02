/**
 * LogStore
 *
 * Responsible for storing and managing log entries in memory.
 * Follows Single Responsibility Principle - handles only storage concerns.
 */

import type { ImageRecognitionDebugData } from "../../types/eventPayloads";

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
  matchedRegionImage?: string; // Base64 encoded cropped region from screenshot at match location
  debug?: ImageRecognitionDebugData; // Debug data with top_matches for displaying match details
  monitorIndex?: number | null; // Which monitor was captured (null = all monitors combined)
}

export interface AiOutputEntry {
  id: string;
  timestamp: number;
  line: string;
  source: string;
  actionId?: string;
  /** Parent task run ID (matches task_runs.id in database) */
  taskRunId?: string;
  /** Session ID for grouping output (may include phase suffix like "-agentic-1") */
  sessionId?: string;
  /** Human-readable session/workflow name (the task title) */
  sessionName?: string;
  /** Workflow phase: setup, verification, agentic, or completion */
  phase?: string;
  /** Iteration number within the phase (1, 2, 3...) */
  phaseIteration?: number;
}

type StoreListener = () => void;

// Caps to prevent unbounded memory growth during long sessions.
// Image logs are capped low because each entry can contain multiple base64
// encoded screenshots (several MB each).
const MAX_GENERAL_LOGS = 5000;
const MAX_IMAGE_LOGS = 200;
const MAX_AI_OUTPUT_LOGS = 5000;

/**
 * Evict the oldest entries from an array when it exceeds maxSize.
 * Removes 20% of entries to avoid evicting on every single add.
 */
function evictOldest<T>(arr: T[], maxSize: number): T[] {
  if (arr.length <= maxSize) return arr;
  const removeCount = Math.floor(maxSize * 0.2);
  return arr.slice(removeCount);
}

/**
 * Strip heavy base64 fields from old image log entries to reclaim memory
 * while preserving metadata for display.
 */
function stripImageData(entry: ImageRecognitionEntry): ImageRecognitionEntry {
  return {
    ...entry,
    screenshotData: undefined,
    visualDebugImage: undefined,
    imageData: undefined,
    matchedRegionImage: undefined,
  };
}

/**
 * Core storage for log entries.
 * Manages in-memory storage and notifies listeners of changes.
 * Enforces size caps to prevent out-of-memory in long-running sessions.
 */
export class LogStore {
  private generalLogs: LogEntry[] = [];
  private imageLogs: ImageRecognitionEntry[] = [];
  private aiOutputLogs: AiOutputEntry[] = [];
  private listeners = new Set<StoreListener>();

  /**
   * Add a general log entry
   */
  addGeneralLog(entry: LogEntry): void {
    this.generalLogs.push(entry);
    if (this.generalLogs.length > MAX_GENERAL_LOGS) {
      this.generalLogs = evictOldest(this.generalLogs, MAX_GENERAL_LOGS);
    }
    this.notifyListeners();
  }

  /**
   * Add an image recognition log entry.
   * When approaching the cap, strips base64 data from older entries first,
   * then evicts the oldest entries entirely.
   */
  addImageLog(entry: ImageRecognitionEntry): void {
    this.imageLogs.push(entry);
    if (this.imageLogs.length > MAX_IMAGE_LOGS) {
      // Strip base64 data from entries in the first half before evicting
      const halfPoint = Math.floor(this.imageLogs.length / 2);
      for (let i = 0; i < halfPoint; i++) {
        this.imageLogs[i] = stripImageData(this.imageLogs[i]);
      }
      this.imageLogs = evictOldest(this.imageLogs, MAX_IMAGE_LOGS);
    }
    this.notifyListeners();
  }

  /**
   * Add an AI output log entry
   */
  addAiOutputLog(entry: AiOutputEntry): void {
    this.aiOutputLogs.push(entry);
    if (this.aiOutputLogs.length > MAX_AI_OUTPUT_LOGS) {
      this.aiOutputLogs = evictOldest(this.aiOutputLogs, MAX_AI_OUTPUT_LOGS);
    }
    this.notifyListeners();
  }

  /**
   * Load multiple AI output log entries at once (for restoring history).
   * Only keeps the most recent entries if the total exceeds the cap.
   */
  loadAiOutputLogs(entries: AiOutputEntry[]): void {
    // Use concat instead of push(...entries) to avoid hitting JS engine
    // argument limits when restoring very large log histories.
    this.aiOutputLogs = this.aiOutputLogs.concat(entries);
    if (this.aiOutputLogs.length > MAX_AI_OUTPUT_LOGS) {
      this.aiOutputLogs = this.aiOutputLogs.slice(-MAX_AI_OUTPUT_LOGS);
    }
    if (entries.length > 0) {
      this.notifyListeners();
    }
  }

  /**
   * Get all general logs (returns a copy for React state compatibility)
   */
  getGeneralLogs(): LogEntry[] {
    return [...this.generalLogs];
  }

  /**
   * Get all image logs (returns a copy for React state compatibility)
   */
  getImageLogs(): ImageRecognitionEntry[] {
    return [...this.imageLogs];
  }

  /**
   * Get all AI output logs (returns a copy for React state compatibility)
   */
  getAiOutputLogs(): AiOutputEntry[] {
    return [...this.aiOutputLogs];
  }

  /**
   * Clear general logs
   */
  clearGeneralLogs(): void {
    this.generalLogs = [];
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
   * Clear AI output logs
   */
  clearAiOutputLogs(): void {
    this.aiOutputLogs = [];
    this.notifyListeners();
  }

  /**
   * Get count of general logs
   */
  getGeneralLogCount(): number {
    return this.generalLogs.length;
  }

  /**
   * Get count of image logs
   */
  getImageLogCount(): number {
    return this.imageLogs.length;
  }

  /**
   * Get count of AI output logs
   */
  getAiOutputLogCount(): number {
    return this.aiOutputLogs.length;
  }

  /**
   * Subscribe to store changes
   * @returns Unsubscribe function
   */
  subscribe(listener: StoreListener): () => void {
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
}
