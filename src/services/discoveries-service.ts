/**
 * Service for discovery sync operations.
 * Wraps Tauri commands for the Discovery Push mechanism.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  DiscoveryResponse,
  DiscoverySummary,
  SyncStatus,
  SyncResult,
  PendingDiscovery,
} from "../types/discoveries";

/**
 * Service for managing discovery sync.
 */
export const discoveriesService = {
  /**
   * Get a summary of pending discoveries for the UI dashboard.
   */
  async getSummary(): Promise<DiscoveryResponse<DiscoverySummary>> {
    try {
      return await invoke<DiscoveryResponse<DiscoverySummary>>("get_discovery_summary");
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to get discovery summary",
      };
    }
  },

  /**
   * Get current sync status.
   */
  async getSyncStatus(): Promise<DiscoveryResponse<SyncStatus>> {
    try {
      return await invoke<DiscoveryResponse<SyncStatus>>("get_discovery_sync_status");
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to get sync status",
      };
    }
  },

  /**
   * Get all pending discoveries.
   */
  async getPendingDiscoveries(): Promise<DiscoveryResponse<PendingDiscovery[]>> {
    try {
      return await invoke<DiscoveryResponse<PendingDiscovery[]>>("get_pending_discoveries_cmd");
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to get pending discoveries",
      };
    }
  },

  /**
   * Trigger sync of pending discoveries to qontinui-web.
   */
  async syncDiscoveries(): Promise<DiscoveryResponse<SyncResult>> {
    try {
      return await invoke<DiscoveryResponse<SyncResult>>("sync_discoveries");
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to sync discoveries",
      };
    }
  },

  /**
   * Clear a specific discovery from the pending queue.
   */
  async clearDiscovery(id: string): Promise<DiscoveryResponse<boolean>> {
    try {
      return await invoke<DiscoveryResponse<boolean>>("clear_discovery", { id });
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to clear discovery",
      };
    }
  },

  /**
   * Clear all failed discoveries (exceeded retry limit).
   */
  async clearFailedDiscoveries(): Promise<DiscoveryResponse<number>> {
    try {
      return await invoke<DiscoveryResponse<number>>("clear_failed_discoveries");
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to clear failed discoveries",
      };
    }
  },
};
