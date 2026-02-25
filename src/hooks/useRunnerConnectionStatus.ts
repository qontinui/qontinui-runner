/**
 * useRunnerConnectionStatus Hook
 *
 * Polls the runner's /health endpoint to determine real connection status.
 * Replaces the hardcoded "Connected" badge in the BottomBar with live data.
 */

import { useState, useEffect, useCallback } from "react";

export interface ConnectionStatus {
  isConnected: boolean;
  latencyMs: number | null;
  lastCheckedAt: number | null;
}

const API_BASE = "http://localhost:9876";
const HEALTH_POLL_INTERVAL = 10_000; // 10 seconds
const HEALTH_TIMEOUT = 5_000; // 5 second timeout

export function useRunnerConnectionStatus(): ConnectionStatus {
  const [status, setStatus] = useState<ConnectionStatus>({
    isConnected: true, // Optimistic — assume connected on mount
    latencyMs: null,
    lastCheckedAt: null,
  });

  const checkHealth = useCallback(async () => {
    const start = performance.now();
    try {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), HEALTH_TIMEOUT);

      const response = await fetch(`${API_BASE}/health`, {
        signal: controller.signal,
      });
      clearTimeout(timeout);

      const latency = Math.round(performance.now() - start);

      setStatus({
        isConnected: response.ok,
        latencyMs: latency,
        lastCheckedAt: Date.now(),
      });
    } catch {
      setStatus({
        isConnected: false,
        latencyMs: null,
        lastCheckedAt: Date.now(),
      });
    }
  }, []);

  useEffect(() => {
    checkHealth();
    const interval = setInterval(checkHealth, HEALTH_POLL_INTERVAL);
    return () => clearInterval(interval);
  }, [checkHealth]);

  return status;
}
