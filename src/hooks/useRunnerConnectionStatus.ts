/**
 * useRunnerConnectionStatus Hook
 *
 * Uses the GraphQL health subscription for real-time connection status.
 * Falls back to REST polling if the subscription isn't available.
 *
 * Migration: replaced 10s setInterval polling of GET /health with
 * the uiBridgeHealthStream GraphQL subscription (5s push).
 */

import { useState, useEffect, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { useHealthStream } from "@/hooks/graphql";

export interface ConnectionStatus {
  isConnected: boolean;
  latencyMs: number | null;
  lastCheckedAt: number | null;
}

const HEALTH_POLL_INTERVAL = 30_000; // 30s fallback (was 10s before subscription)
const HEALTH_TIMEOUT = 5_000;

/**
 * Primary implementation: GraphQL subscription-based health monitoring.
 * The subscription pushes health data every 5s over WebSocket,
 * eliminating HTTP polling overhead entirely.
 */
export function useRunnerConnectionStatus(): ConnectionStatus {
  const { data, error: subError } = useHealthStream(5000);
  const [fallbackStatus, setFallbackStatus] = useState<ConnectionStatus | null>(null);

  // If subscription is delivering data, derive status from it
  if (data?.uiBridgeHealthStream) {
    const health = data.uiBridgeHealthStream;
    return {
      isConnected: health.responsive,
      latencyMs: parseInt(health.heartbeatAgeMs, 10) || null,
      lastCheckedAt: Date.now(),
    };
  }

  // Fallback: poll REST endpoint if subscription not connected yet
  const checkHealth = useCallback(async () => {
    const start = performance.now();
    try {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), HEALTH_TIMEOUT);
      const response = await tracedFetch(`${getApiBase()}/health`, {
        signal: controller.signal,
      });
      clearTimeout(timeout);
      const latency = Math.round(performance.now() - start);
      setFallbackStatus({
        isConnected: response.ok,
        latencyMs: latency,
        lastCheckedAt: Date.now(),
      });
    } catch {
      setFallbackStatus({
        isConnected: false,
        latencyMs: null,
        lastCheckedAt: Date.now(),
      });
    }
  }, []);

  // Only start fallback polling if subscription has an error or no data yet
  useEffect(() => {
    if (data?.uiBridgeHealthStream) return; // Subscription is working
    checkHealth();
    const interval = setInterval(checkHealth, HEALTH_POLL_INTERVAL);
    return () => clearInterval(interval);
  }, [checkHealth, data, subError]);

  return fallbackStatus ?? {
    isConnected: !subError, // Optimistic if no data yet
    latencyMs: null,
    lastCheckedAt: null,
  };
}
