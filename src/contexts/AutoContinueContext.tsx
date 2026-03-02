/**
 * AutoContinueContext.tsx
 *
 * React Context for managing the Auto-Continue on Restart setting.
 * This provides a single source of truth that syncs across all components
 * without polling or race conditions.
 */

import { createContext, useContext, useState, useCallback, useEffect, type ReactNode } from "react";
import { getApiPort } from "@/lib/runner-api";

interface AutoContinueContextValue {
  /** Whether auto-continue on restart is enabled */
  enabled: boolean;
  /** Whether a toggle operation is in progress */
  loading: boolean;
  /** Toggle the auto-continue setting */
  toggle: () => Promise<void>;
  /** Set the auto-continue setting to a specific value */
  setEnabled: (value: boolean) => Promise<void>;
}

const AutoContinueContext = createContext<AutoContinueContextValue | null>(null);

interface AutoContinueProviderProps {
  children: ReactNode;
}

/** Build port list: primary port from config + two consecutive fallbacks for zombie connections on Windows */
function getMcpApiPorts(): number[] {
  const primary = getApiPort();
  return [primary, primary + 1, primary + 2];
}

/** Fetch with timeout */
async function fetchWithTimeout(
  url: string,
  options: RequestInit = {},
  timeoutMs: number = 5000,
): Promise<Response> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetch(url, {
      ...options,
      signal: controller.signal,
    });
    return response;
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * Try to fetch from multiple ports, returning the first successful response.
 * Caches the working port for subsequent requests.
 */
let cachedPort: number | null = null;

async function fetchWithFallbackPorts(
  path: string,
  options: RequestInit = {},
  timeoutMs: number = 2000,
): Promise<Response> {
  // If we have a cached working port, try it first
  if (cachedPort !== null) {
    try {
      const response = await fetchWithTimeout(
        `http://localhost:${cachedPort}${path}`,
        options,
        timeoutMs,
      );
      return response;
    } catch {
      // Port no longer working, clear cache and try all ports
      cachedPort = null;
    }
  }

  // Try all ports
  for (const port of getMcpApiPorts()) {
    try {
      const response = await fetchWithTimeout(
        `http://localhost:${port}${path}`,
        options,
        timeoutMs,
      );
      // Cache the working port
      cachedPort = port;
      return response;
    } catch {
      // Try next port
    }
  }

  throw new Error(`Could not connect to MCP API on any port: ${getMcpApiPorts().join(", ")}`);
}

/** Maximum number of retry attempts when fetching initial state */
const MAX_INIT_RETRIES = 5;
/** Delay between retry attempts in ms */
const RETRY_DELAY_MS = 1000;
/** Interval for periodic sync with backend (ms) */
const SYNC_INTERVAL_MS = 10000;

export function AutoContinueProvider({ children }: AutoContinueProviderProps) {
  // Default to true to match backend default (settings.rs: default_auto_continue_ai_workflow)
  const [enabled, setEnabledState] = useState(true);
  const [loading, setLoading] = useState(false);
  const [initialized, setInitialized] = useState(false);

  // Fetch current value from server (returns success boolean)
  const fetchCurrentValue = useCallback(
    async (clearCache: boolean = false): Promise<boolean> => {
      try {
        if (clearCache) {
          cachedPort = null;
        }
        const response = await fetchWithFallbackPorts("/workflow/auto-continue", {}, 2000);
        const result = await response.json();
        if (result.success && result.data) {
          setEnabledState(result.data.enabled);
          return true;
        }
        return false;
      } catch (error) {
        // Only warn on initial fetch, not during periodic sync
        if (!initialized) {
          console.warn("Could not fetch auto-continue setting:", error);
        }
        return false;
      }
    },
    [initialized],
  );

  // Fetch initial value on mount with retry logic
  // This handles the case where the server isn't ready immediately after restart
  useEffect(() => {
    let cancelled = false;
    let retryCount = 0;

    const initWithRetry = async () => {
      while (retryCount < MAX_INIT_RETRIES && !cancelled) {
        // Clear cache on first attempt to ensure we find the right server after restart
        const success = await fetchCurrentValue(retryCount === 0);
        if (success) {
          if (!cancelled) {
            setInitialized(true);
          }
          return;
        }

        retryCount++;
        if (retryCount < MAX_INIT_RETRIES && !cancelled) {
          // Wait before retrying (server might be starting up)
          await new Promise((resolve) => setTimeout(resolve, RETRY_DELAY_MS));
        }
      }

      // After all retries, still mark as initialized (with default true)
      if (!cancelled) {
        console.warn(
          `Auto-continue: Failed to fetch after ${MAX_INIT_RETRIES} attempts, using default (true)`,
        );
        setInitialized(true);
      }
    };

    initWithRetry();

    return () => {
      cancelled = true;
    };
  }, [fetchCurrentValue]);

  // Periodic sync to keep UI in sync with backend
  // This handles cases where the runner restarts or settings change externally
  useEffect(() => {
    if (!initialized) return;

    let intervalId: ReturnType<typeof setInterval>;
    let cancelled = false;

    const sync = async () => {
      if (cancelled || loading) return;
      await fetchCurrentValue(false);
    };

    // Start periodic sync
    intervalId = setInterval(sync, SYNC_INTERVAL_MS);

    // Also sync when the tab becomes visible (user returns to tab)
    const handleVisibilityChange = () => {
      if (!document.hidden && !cancelled && !loading) {
        // Clear port cache when tab becomes visible in case runner restarted
        cachedPort = null;
        fetchCurrentValue(false);
      }
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      cancelled = true;
      clearInterval(intervalId);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [initialized, loading, fetchCurrentValue]);

  // Set to a specific value
  const setEnabled = useCallback(
    async (value: boolean) => {
      // Prevent multiple concurrent toggles
      if (loading) {
        console.warn("Auto-continue toggle already in progress");
        return;
      }

      // Optimistic update
      const previousValue = enabled;
      setEnabledState(value);
      setLoading(true);

      try {
        const response = await fetchWithFallbackPorts(
          "/workflow/auto-continue",
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ enabled: value }),
          },
          5000,
        );
        const result = await response.json();
        if (result.success && result.data) {
          // Confirm with server response
          setEnabledState(result.data.enabled);
        } else {
          // Rollback on failure
          console.error("Failed to set auto-continue:", result.error);
          setEnabledState(previousValue);
        }
      } catch (error) {
        // Rollback on error
        if (error instanceof Error && error.name === "AbortError") {
          console.error("Auto-continue toggle timed out");
        } else {
          console.error("Failed to set auto-continue setting:", error);
        }
        setEnabledState(previousValue);
      } finally {
        setLoading(false);
      }
    },
    [enabled, loading],
  );

  // Toggle the current value
  const toggle = useCallback(async () => {
    await setEnabled(!enabled);
  }, [enabled, setEnabled]);

  // Don't render children until initialized to prevent flash of wrong state
  if (!initialized) {
    return null;
  }

  return (
    <AutoContinueContext.Provider value={{ enabled, loading, toggle, setEnabled }}>
      {children}
    </AutoContinueContext.Provider>
  );
}

/**
 * Hook to access the auto-continue context.
 * Must be used within an AutoContinueProvider.
 */
export function useAutoContinue(): AutoContinueContextValue {
  const context = useContext(AutoContinueContext);
  if (!context) {
    throw new Error("useAutoContinue must be used within an AutoContinueProvider");
  }
  return context;
}
