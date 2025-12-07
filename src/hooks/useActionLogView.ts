/**
 * useActionLogView Hook
 *
 * React hook for fetching and managing Action Log view data from the display profile system.
 * Provides automatic state management, error handling, and optional auto-refresh.
 *
 * @example
 * ```tsx
 * function ActionsTab() {
 *   const { viewData, loading, error, refresh } = useActionLogView({
 *     autoRefreshInterval: 1000, // Refresh every second during execution
 *   });
 *
 *   if (loading) return <div>Loading actions...</div>;
 *   if (error) return <div>Error: {error}</div>;
 *   if (!viewData) return <div>No data</div>;
 *
 *   return (
 *     <div>
 *       <p>Showing {viewData.visible_count} of {viewData.total_count} actions</p>
 *       {viewData.actions.map(action => (
 *         <ActionRow key={action.id} action={action} />
 *       ))}
 *     </div>
 *   );
 * }
 * ```
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ActionLogViewData, CommandResponse } from "../types/displayProfile";
import { actionLogManager } from "../managers";

export interface UseActionLogViewOptions {
  /**
   * Auto-refresh interval in milliseconds.
   * If set, the hook will automatically refresh data at this interval.
   * Set to 0 or undefined to disable auto-refresh.
   */
  autoRefreshInterval?: number;

  /**
   * Whether to fetch data immediately on mount.
   * @default true
   */
  fetchOnMount?: boolean;
}

export interface UseActionLogViewResult {
  /** The action log view data, or null if not yet loaded */
  viewData: ActionLogViewData | null;

  /** Whether data is currently being fetched */
  loading: boolean;

  /** Error message if the fetch failed, or null if no error */
  error: string | null;

  /** Function to manually trigger a data refresh */
  refresh: () => Promise<void>;
}

/**
 * Hook for fetching and managing Action Log view data.
 *
 * @param options - Configuration options for the hook
 * @returns Object containing view data, loading state, error state, and refresh function
 */
export function useActionLogView(options: UseActionLogViewOptions = {}): UseActionLogViewResult {
  const { autoRefreshInterval, fetchOnMount = true } = options;

  const [viewData, setViewData] = useState<ActionLogViewData | null>(null);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  // Use ref to track if component is mounted (prevents state updates after unmount)
  const isMountedRef = useRef<boolean>(true);

  /**
   * Fetch action log view data from Tauri backend.
   */
  const fetchData = useCallback(async () => {
    console.log("[HOOK] fetchData called, isMountedRef.current:", isMountedRef.current);

    // Don't fetch if component is unmounted
    if (!isMountedRef.current) {
      console.log("[HOOK] Component unmounted, aborting fetch");
      return;
    }

    console.log("[HOOK] Fetching action log view...");
    setLoading(true);
    setError(null);

    try {
      const response = await invoke<CommandResponse<ActionLogViewData>>("get_action_log_view");

      console.log("[HOOK] Response received:", response);

      // Only update state if component is still mounted
      if (!isMountedRef.current) {
        return;
      }

      if (response.success && response.data) {
        console.log("[HOOK] Setting viewData:", response.data);
        setViewData(response.data);
        setError(null);
      } else {
        const errorMessage = response.message || "Failed to fetch action log view";
        console.log("[HOOK] Error - response not successful:", errorMessage);
        setError(errorMessage);
        console.error("get_action_log_view failed:", errorMessage);
      }
    } catch (err) {
      // Only update state if component is still mounted
      if (!isMountedRef.current) {
        return;
      }

      // Tauri commands return string errors when Rust returns Err(String)
      const errorMessage = err instanceof Error ? err.message : String(err);
      console.log("[HOOK] Exception caught:", errorMessage, "Raw error:", err);
      setError(errorMessage);
      console.error("Failed to fetch action log view:", err);
    } finally {
      // Only update state if component is still mounted
      if (isMountedRef.current) {
        console.log("[HOOK] Fetch complete, setting loading=false");
        setLoading(false);
      }
    }
  }, []);

  /**
   * Public refresh function that can be called manually.
   */
  const refresh = useCallback(async () => {
    console.log("[HOOK] refresh() called");
    await fetchData();
  }, [fetchData]);

  // Fetch data on mount if enabled
  useEffect(() => {
    if (fetchOnMount) {
      fetchData();
    }
  }, [fetchOnMount, fetchData]);

  // Set up auto-refresh if interval is specified
  useEffect(() => {
    if (!autoRefreshInterval || autoRefreshInterval <= 0) {
      return;
    }

    const intervalId = setInterval(() => {
      fetchData();
    }, autoRefreshInterval);

    // Cleanup interval on unmount or when interval changes
    return () => {
      clearInterval(intervalId);
    };
  }, [autoRefreshInterval, fetchData]);

  // Subscribe to ActionLogManager refresh triggers (e.g., from tree events)
  useEffect(() => {
    console.log("[HOOK] Subscribing to ActionLogManager refresh triggers");
    const unsubscribe = actionLogManager.onRefreshNeeded(() => {
      console.log("[HOOK] ActionLogManager triggered refresh");
      refresh();
    });

    return () => {
      console.log("[HOOK] Unsubscribing from ActionLogManager");
      unsubscribe();
    };
  }, [refresh]);

  // Track mount/unmount status
  useEffect(() => {
    // Set to true on mount (handles React StrictMode double-mounting)
    isMountedRef.current = true;

    // Set to false on unmount
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  return {
    viewData,
    loading,
    error,
    refresh,
  };
}
