/**
 * ReportPersistenceService
 *
 * Unified service for report and findings persistence.
 * Combines local file persistence with backend synchronization,
 * handles offline mode gracefully, and queues failed syncs for retry.
 */

import type { Finding, ExecutionReport } from "../types/findings";
import type { SessionHistoryEntry } from "../findings/types";
import {
  syncFindingsToBackend,
  syncReportToBackend,
  checkBackendHealth,
  type BackendSyncOptions,
} from "../findings/FindingsSync";
import { persistFindingsData, loadFindingsData } from "../findings/FindingsPersistence";

// ============================================================================
// Types
// ============================================================================

/**
 * Backend configuration for sync operations
 */
export interface BackendConfig {
  baseUrl: string;
  projectId: string;
  authToken?: string;
  timeout?: number;
}

/**
 * Result of a sync operation
 */
export interface SyncResult {
  success: boolean;
  syncedCount: number;
  errors: string[];
}

/**
 * An item pending synchronization
 */
export interface PendingSyncItem {
  id: string;
  type: "report" | "findings";
  data: ExecutionReport | Finding[];
  attempts: number;
  lastAttempt: number;
  error?: string;
}

/**
 * Callback type for sync status changes
 */
type SyncStatusCallback = (pending: number) => void;

// ============================================================================
// Constants
// ============================================================================

const PENDING_SYNCS_STORAGE_KEY = "qontinui-pending-syncs";
const BACKEND_CONFIG_STORAGE_KEY = "qontinui-backend-config";
const MAX_RETRY_ATTEMPTS = 5;
const RETRY_DELAY_BASE_MS = 1000; // Exponential backoff base

// ============================================================================
// ReportPersistenceService Class
// ============================================================================

export class ReportPersistenceService {
  private static instance: ReportPersistenceService;

  private backendConfig: BackendConfig | null = null;
  private pendingSyncs: PendingSyncItem[] = [];
  private statusCallbacks: Set<SyncStatusCallback> = new Set();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private isRetrying = false;

  private constructor() {
    this.loadPersistedState();
    this.scheduleRetry();
  }

  /**
   * Get the singleton instance
   */
  static getInstance(): ReportPersistenceService {
    if (!ReportPersistenceService.instance) {
      ReportPersistenceService.instance = new ReportPersistenceService();
    }
    return ReportPersistenceService.instance;
  }

  // ==========================================================================
  // Configuration
  // ==========================================================================

  /**
   * Set the backend configuration for sync operations
   * Pass null to disable backend sync (offline mode)
   */
  setBackendConfig(config: BackendConfig | null): void {
    this.backendConfig = config;

    // Persist to localStorage
    if (config) {
      localStorage.setItem(BACKEND_CONFIG_STORAGE_KEY, JSON.stringify(config));
      console.log(
        `[ReportPersistence] Backend configured: ${config.baseUrl} (project: ${config.projectId})`,
      );
    } else {
      localStorage.removeItem(BACKEND_CONFIG_STORAGE_KEY);
      console.log("[ReportPersistence] Backend sync disabled (offline mode)");
    }

    // Try pending syncs if we now have a config
    if (config && this.pendingSyncs.length > 0) {
      this.scheduleRetry();
    }
  }

  /**
   * Get the current backend configuration
   */
  getBackendConfig(): BackendConfig | null {
    return this.backendConfig ? { ...this.backendConfig } : null;
  }

  /**
   * Check if backend sync is configured
   */
  isBackendConfigured(): boolean {
    return this.backendConfig !== null;
  }

  // ==========================================================================
  // Backend Health
  // ==========================================================================

  /**
   * Check if the backend is reachable
   */
  async checkBackendHealth(): Promise<boolean> {
    if (!this.backendConfig) {
      return false;
    }

    try {
      return await checkBackendHealth(this.backendConfig.baseUrl);
    } catch (error) {
      console.warn("[ReportPersistence] Backend health check failed:", error);
      return false;
    }
  }

  // ==========================================================================
  // Report Persistence
  // ==========================================================================

  /**
   * Save a report to both local storage and backend
   * If backend sync fails, queues for retry
   */
  async saveReport(report: ExecutionReport): Promise<SyncResult> {
    const result: SyncResult = {
      success: false,
      syncedCount: 0,
      errors: [],
    };

    // If no backend configured, return success (offline mode)
    if (!this.backendConfig) {
      console.log("[ReportPersistence] No backend configured, skipping sync");
      result.success = true;
      return result;
    }

    try {
      const syncOptions = this.toSyncOptions();
      const backendResult = await syncReportToBackend(report, syncOptions);

      if (backendResult.success) {
        result.success = true;
        result.syncedCount = 1;
        console.log(`[ReportPersistence] Report ${report.id} synced to backend`);
      } else {
        // Queue for retry
        this.addToPendingQueue("report", report, backendResult.errors.join("; "));
        result.errors = backendResult.errors;
      }
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      this.addToPendingQueue("report", report, errorMessage);
      result.errors.push(errorMessage);
      console.error("[ReportPersistence] Report sync failed, queued for retry:", error);
    }

    return result;
  }

  /**
   * Save findings to both local storage and backend
   * If backend sync fails, queues for retry
   */
  async saveFindings(findings: Finding[]): Promise<SyncResult> {
    const result: SyncResult = {
      success: false,
      syncedCount: 0,
      errors: [],
    };

    if (findings.length === 0) {
      result.success = true;
      return result;
    }

    // If no backend configured, return success (offline mode)
    if (!this.backendConfig) {
      console.log("[ReportPersistence] No backend configured, skipping sync");
      result.success = true;
      return result;
    }

    try {
      const syncOptions = this.toSyncOptions();
      const backendResult = await syncFindingsToBackend(findings, syncOptions);

      if (backendResult.success) {
        result.success = true;
        result.syncedCount = backendResult.syncedCount;
        console.log(`[ReportPersistence] ${result.syncedCount} findings synced to backend`);
      } else {
        // Queue for retry
        this.addToPendingQueue("findings", findings, backendResult.errors.join("; "));
        result.errors = backendResult.errors;
      }
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      this.addToPendingQueue("findings", findings, errorMessage);
      result.errors.push(errorMessage);
      console.error("[ReportPersistence] Findings sync failed, queued for retry:", error);
    }

    return result;
  }

  // ==========================================================================
  // Local Persistence (delegates to FindingsPersistence)
  // ==========================================================================

  /**
   * Save report and findings to local disk via Tauri
   */
  async saveToLocal(report: ExecutionReport, findings: Finding[]): Promise<void> {
    try {
      // Build session history from report
      const sessionHistory: SessionHistoryEntry[] = [];

      await persistFindingsData({
        currentSessionId: report.sessionId,
        findings,
        reports: [report],
        sessionHistory,
      });

      console.log("[ReportPersistence] Saved to local disk");
    } catch (error) {
      console.error("[ReportPersistence] Failed to save to local disk:", error);
      throw error;
    }
  }

  /**
   * Load reports and findings from local disk
   */
  async loadFromLocal(): Promise<{ reports: ExecutionReport[]; findings: Finding[] }> {
    try {
      const data = await loadFindingsData();

      if (!data) {
        return { reports: [], findings: [] };
      }

      console.log(
        `[ReportPersistence] Loaded from disk: ${data.reports?.length || 0} reports, ${data.findings?.length || 0} findings`,
      );

      return {
        reports: data.reports || [],
        findings: data.findings || [],
      };
    } catch (error) {
      console.error("[ReportPersistence] Failed to load from local disk:", error);
      return { reports: [], findings: [] };
    }
  }

  // ==========================================================================
  // Retry Queue
  // ==========================================================================

  /**
   * Get all pending sync items
   */
  getPendingSyncs(): PendingSyncItem[] {
    return [...this.pendingSyncs];
  }

  /**
   * Retry all pending syncs
   * Returns results for each retry attempt
   */
  async retryPendingSyncs(): Promise<SyncResult[]> {
    if (!this.backendConfig) {
      console.log("[ReportPersistence] No backend configured, cannot retry syncs");
      return [];
    }

    if (this.isRetrying) {
      console.log("[ReportPersistence] Retry already in progress");
      return [];
    }

    if (this.pendingSyncs.length === 0) {
      return [];
    }

    this.isRetrying = true;
    const results: SyncResult[] = [];
    const successfulIds: string[] = [];

    console.log(`[ReportPersistence] Retrying ${this.pendingSyncs.length} pending syncs`);

    for (const item of this.pendingSyncs) {
      // Skip items that have exceeded max retries
      if (item.attempts >= MAX_RETRY_ATTEMPTS) {
        console.warn(
          `[ReportPersistence] Item ${item.id} exceeded max retries (${MAX_RETRY_ATTEMPTS}), skipping`,
        );
        continue;
      }

      // Update attempt count
      item.attempts++;
      item.lastAttempt = Date.now();

      let result: SyncResult;

      try {
        const syncOptions = this.toSyncOptions();

        if (item.type === "report") {
          result = await syncReportToBackend(item.data as ExecutionReport, syncOptions);
        } else {
          result = await syncFindingsToBackend(item.data as Finding[], syncOptions);
        }

        if (result.success) {
          successfulIds.push(item.id);
          console.log(`[ReportPersistence] Successfully retried ${item.type} ${item.id}`);
        } else {
          item.error = result.errors.join("; ");
        }

        results.push(result);
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        item.error = errorMessage;
        results.push({
          success: false,
          syncedCount: 0,
          errors: [errorMessage],
        });
      }
    }

    // Remove successful items from queue
    this.pendingSyncs = this.pendingSyncs.filter((item) => !successfulIds.includes(item.id));

    // Also remove items that have exceeded max retries
    this.pendingSyncs = this.pendingSyncs.filter((item) => item.attempts < MAX_RETRY_ATTEMPTS);

    this.persistPendingSyncs();
    this.notifyStatusChange();

    this.isRetrying = false;

    // Schedule another retry if there are still pending items
    if (this.pendingSyncs.length > 0) {
      this.scheduleRetry();
    }

    return results;
  }

  /**
   * Clear all pending syncs (use with caution - data will be lost)
   */
  clearPendingSyncs(): void {
    const count = this.pendingSyncs.length;
    this.pendingSyncs = [];
    this.persistPendingSyncs();
    this.notifyStatusChange();

    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }

    console.log(`[ReportPersistence] Cleared ${count} pending syncs`);
  }

  // ==========================================================================
  // Events
  // ==========================================================================

  /**
   * Subscribe to sync status changes
   * Returns an unsubscribe function
   */
  onSyncStatusChange(callback: SyncStatusCallback): () => void {
    this.statusCallbacks.add(callback);

    // Immediately notify of current state
    callback(this.pendingSyncs.length);

    return () => {
      this.statusCallbacks.delete(callback);
    };
  }

  // ==========================================================================
  // Private Helpers
  // ==========================================================================

  /**
   * Convert BackendConfig to BackendSyncOptions
   */
  private toSyncOptions(): BackendSyncOptions {
    if (!this.backendConfig) {
      throw new Error("Backend not configured");
    }

    return {
      baseUrl: this.backendConfig.baseUrl,
      projectId: this.backendConfig.projectId,
      authToken: this.backendConfig.authToken,
      timeout: this.backendConfig.timeout,
    };
  }

  /**
   * Add an item to the pending sync queue
   */
  private addToPendingQueue(
    type: "report" | "findings",
    data: ExecutionReport | Finding[],
    error?: string,
  ): void {
    const id = crypto.randomUUID();

    const item: PendingSyncItem = {
      id,
      type,
      data,
      attempts: 1,
      lastAttempt: Date.now(),
      error,
    };

    this.pendingSyncs.push(item);
    this.persistPendingSyncs();
    this.notifyStatusChange();
    this.scheduleRetry();

    console.log(
      `[ReportPersistence] Queued ${type} for retry (${this.pendingSyncs.length} pending)`,
    );
  }

  /**
   * Schedule a retry attempt with exponential backoff
   */
  private scheduleRetry(): void {
    if (this.retryTimer) {
      return; // Already scheduled
    }

    if (this.pendingSyncs.length === 0) {
      return; // Nothing to retry
    }

    if (!this.backendConfig) {
      return; // No backend configured
    }

    // Calculate delay based on oldest item's attempt count
    const maxAttempts = Math.max(...this.pendingSyncs.map((item) => item.attempts));
    const delay = Math.min(
      RETRY_DELAY_BASE_MS * Math.pow(2, maxAttempts),
      60000, // Max 1 minute
    );

    console.log(`[ReportPersistence] Scheduling retry in ${delay}ms`);

    this.retryTimer = setTimeout(async () => {
      this.retryTimer = null;
      await this.retryPendingSyncs();
    }, delay);
  }

  /**
   * Persist pending syncs to localStorage
   */
  private persistPendingSyncs(): void {
    try {
      localStorage.setItem(PENDING_SYNCS_STORAGE_KEY, JSON.stringify(this.pendingSyncs));
    } catch (error) {
      console.error("[ReportPersistence] Failed to persist pending syncs:", error);
    }
  }

  /**
   * Load persisted state from localStorage
   */
  private loadPersistedState(): void {
    // Load backend config
    try {
      const configJson = localStorage.getItem(BACKEND_CONFIG_STORAGE_KEY);
      if (configJson) {
        this.backendConfig = JSON.parse(configJson);
        console.log(`[ReportPersistence] Loaded backend config: ${this.backendConfig?.baseUrl}`);
      }
    } catch (error) {
      console.error("[ReportPersistence] Failed to load backend config:", error);
    }

    // Load pending syncs
    try {
      const syncsJson = localStorage.getItem(PENDING_SYNCS_STORAGE_KEY);
      if (syncsJson) {
        this.pendingSyncs = JSON.parse(syncsJson);
        console.log(`[ReportPersistence] Loaded ${this.pendingSyncs.length} pending syncs`);
      }
    } catch (error) {
      console.error("[ReportPersistence] Failed to load pending syncs:", error);
      this.pendingSyncs = [];
    }
  }

  /**
   * Notify all listeners of a status change
   */
  private notifyStatusChange(): void {
    const count = this.pendingSyncs.length;
    this.statusCallbacks.forEach((callback) => {
      try {
        callback(count);
      } catch (error) {
        console.error("[ReportPersistence] Status callback error:", error);
      }
    });
  }
}

// ============================================================================
// Singleton Export
// ============================================================================

/**
 * Default singleton instance of ReportPersistenceService
 */
export const reportPersistenceService = ReportPersistenceService.getInstance();
