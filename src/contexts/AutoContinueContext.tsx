/**
 * AutoContinueContext.tsx
 *
 * React Context for managing the Auto-Continue on Restart setting.
 * This provides a single source of truth that syncs across all components
 * without polling or race conditions.
 */

import { createContext, useContext, useState, useCallback, useEffect, type ReactNode } from "react";

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

/** Ports to try for the MCP API server (fallback for zombie connections on Windows) */
const MCP_API_PORTS = [9876, 9877, 9878];

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
  for (const port of MCP_API_PORTS) {
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

  throw new Error(`Could not connect to MCP API on any port: ${MCP_API_PORTS.join(", ")}`);
}

export function AutoContinueProvider({ children }: AutoContinueProviderProps) {
  const [enabled, setEnabledState] = useState(false);
  const [loading, setLoading] = useState(false);
  const [initialized, setInitialized] = useState(false);

  // Fetch initial value on mount
  useEffect(() => {
    const fetchInitialValue = async () => {
      try {
        const response = await fetchWithFallbackPorts("/workflow/auto-continue", {}, 2000);
        const result = await response.json();
        if (result.success && result.data) {
          setEnabledState(result.data.enabled);
        }
      } catch (error) {
        // Silently fail - runner might not be running yet
        console.warn("Could not fetch auto-continue setting:", error);
      } finally {
        setInitialized(true);
      }
    };
    fetchInitialValue();
  }, []);

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
