/**
 * useRunnerConnectionStatus Hook
 *
 * Uses the GraphQL health subscription for real-time connection status.
 * Falls back to REST polling if the subscription isn't available.
 *
 * Migration: replaced 10s setInterval polling of GET /health with
 * the uiBridgeHealthStream GraphQL subscription (5s push).
 */

import { useState, useEffect, useCallback, useMemo } from "react";
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

  // Track current time for subscription status; refresh when data changes
  const [lastDataTime, setLastDataTime] = useState(() => Date.now());
  useEffect(() => {
    if (data?.uiBridgeHealthStream) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- sync time snapshot when subscription data arrives
      setLastDataTime(Date.now());
    }
  }, [data]);

  // Derive subscription status (all hooks called unconditionally above)
  const subscriptionStatus = useMemo<ConnectionStatus | null>(() => {
    if (!data?.uiBridgeHealthStream) return null;
    const health = data.uiBridgeHealthStream;
    return {
      isConnected: health.responsive,
      latencyMs: parseInt(health.heartbeatAgeMs, 10) || null,
      lastCheckedAt: lastDataTime,
    };
  }, [data, lastDataTime]);

  const hasSubscriptionData = !!subscriptionStatus;

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

  // Only start fallback polling if subscription has no data yet
  useEffect(() => {
    if (hasSubscriptionData) return; // Subscription is working, skip polling
    // eslint-disable-next-line react-hooks/set-state-in-effect -- health polling fallback
    checkHealth();
    const interval = setInterval(checkHealth, HEALTH_POLL_INTERVAL);
    return () => clearInterval(interval);
  }, [checkHealth, hasSubscriptionData, subError]);

  // Return subscription data if available, otherwise fallback
  return (
    subscriptionStatus ??
    fallbackStatus ?? {
      isConnected: !subError, // Optimistic if no data yet
      latencyMs: null,
      lastCheckedAt: null,
    }
  );
}
