/**
 * useGuiLock Hook
 *
 * Provides access to the GUI lock state for multi-bridge scenarios.
 * Polls the runner API to track which bridge holds exclusive GUI access.
 */

import { useState, useEffect, useCallback } from "react";
import { getApiBase } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 2000;

/**
 * GUI lock information from the API.
 */
export interface GuiLockState {
  /** ID of the bridge currently holding the lock */
  holderId: string | null;
  /** Timestamp when the lock was acquired */
  acquiredAt: number | null;
  /** Whether the lock is currently held */
  isLocked: boolean;
}

/**
 * Result of the useGuiLock hook.
 */
export interface UseGuiLockResult {
  /** Current GUI lock state */
  state: GuiLockState;
  /** Whether loading initial data */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Check if a specific bridge holds the lock */
  isHeldBy: (bridgeId: string) => boolean;
  /** Refresh lock state */
  refresh: () => Promise<void>;
}

/**
 * Hook to track GUI lock state.
 *
 * @param enabled Whether to poll for updates (default: true)
 * @returns GUI lock state and helpers
 */
export function useGuiLock(enabled = true): UseGuiLockResult {
  const [state, setState] = useState<GuiLockState>({
    holderId: null,
    acquiredAt: null,
    isLocked: false,
  });
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  /**
   * Fetch GUI lock state from API.
   */
  const refresh = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/gui-lock`);

      if (!response.ok) {
        // API may return error if bridge manager not initialized
        setState({ holderId: null, acquiredAt: null, isLocked: false });
        return;
      }

      const data = await response.json();

      if (data.success && data.data) {
        setState({
          holderId: data.data.holder_id ?? null,
          acquiredAt: data.data.acquired_at ?? null,
          isLocked: !!data.data.holder_id,
        });
        setError(null);
      } else {
        setState({ holderId: null, acquiredAt: null, isLocked: false });
        if (data.error) {
          setError(data.error);
        }
      }
    } catch (e) {
      // API may not be available
      setState({ holderId: null, acquiredAt: null, isLocked: false });
      if (e instanceof Error) {
        setError(e.message);
      }
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Initial fetch and polling
  useEffect(() => {
    if (!enabled) {
      setIsLoading(false);
      return;
    }

    refresh();
    const interval = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [enabled, refresh]);

  /**
   * Check if a specific bridge holds the lock.
   */
  const isHeldBy = useCallback(
    (bridgeId: string) => {
      return state.holderId === bridgeId;
    },
    [state.holderId],
  );

  return {
    state,
    isLoading,
    error,
    isHeldBy,
    refresh,
  };
}

export default useGuiLock;
