/**
 * LogStore
 *
 * Responsible for storing and managing log entries in memory.
 * Follows Single Responsibility Principle - handles only storage concerns.
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
  matchedRegionImage?: string; // Base64 encoded cropped region from screenshot at match location
  debug?: any; // Debug data with top_matches for displaying match details
}

export interface AiOutputEntry {
  id: string;
  timestamp: number;
  line: string;
  source: string;
  actionId?: string;
}

type StoreListener = () => void;

/**
 * Core storage for log entries.
 * Manages in-memory storage and notifies listeners of changes.
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
   * Add an AI output log entry
   */
  addAiOutputLog(entry: AiOutputEntry): void {
    this.aiOutputLogs.push(entry);
    this.notifyListeners();
  }

  /**
   * Get all general logs (returns a copy)
   */
  getGeneralLogs(): LogEntry[] {
    return [...this.generalLogs];
  }

  /**
   * Get all image logs (returns a copy)
   */
  getImageLogs(): ImageRecognitionEntry[] {
    return [...this.imageLogs];
  }

  /**
   * Get all AI output logs (returns a copy)
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
