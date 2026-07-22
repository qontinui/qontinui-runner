/**
 * useRunnerConnectionStatus Hook
 *
 * Uses the GraphQL health subscription for real-time connection status.
 * Falls back to REST polling if the subscription isn't available.
 *
 * Migration: replaced 10s setInterval polling of GET /health with
 * the uiBridgeHealthStream GraphQL subscription (5s push).
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { useHealthStream } from "@/hooks/graphql";

export interface ConnectionStatus {
  isConnected: boolean;
  latencyMs: number | null;
  lastCheckedAt: number | null;
  /**
   * NO-DOWNGRADE: true while we have not yet established connectivity either
   * way — i.e. the fallback poll has failed at least once but has not yet hit
   * [`HEALTH_FAILURE_THRESHOLD`] consecutive failures. Render this as
   * "checking…"/"unknown", never as "Disconnected".
   */
  isUnknown: boolean;
  /** Consecutive failed fallback health checks (0 while healthy). */
  consecutiveFailures: number;
}

const HEALTH_POLL_INTERVAL = 30_000; // 30s fallback (was 10s before subscription)
const HEALTH_TIMEOUT = 5_000;
/**
 * NO-DOWNGRADE (L2): a SINGLE failed health fetch used to flip the indicator
 * straight to "Disconnected" — one dropped request, a GC pause, or a 5s
 * timeout under load and the user is told the runner is down. One failure
 * means UNKNOWN; only a run of them means disconnected.
 */
const HEALTH_FAILURE_THRESHOLD = 3;

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
      isUnknown: false,
      consecutiveFailures: 0,
    };
  }, [data, lastDataTime]);

  const hasSubscriptionData = !!subscriptionStatus;
  const failureCountRef = useRef(0);

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
      if (response.ok) {
        failureCountRef.current = 0;
        setFallbackStatus({
          isConnected: true,
          latencyMs: latency,
          lastCheckedAt: Date.now(),
          isUnknown: false,
          consecutiveFailures: 0,
        });
        return;
      }
      // A non-2xx counts as a failure, same as a throw.
      failureCountRef.current += 1;
    } catch {
      failureCountRef.current += 1;
    }
    const failures = failureCountRef.current;
    const settled = failures >= HEALTH_FAILURE_THRESHOLD;
    setFallbackStatus((prev) => ({
      // Below the threshold, hold the last known verdict (optimistic on first
      // boot) rather than declaring the runner down on one dropped request.
      isConnected: settled ? false : (prev?.isConnected ?? true),
      latencyMs: null,
      lastCheckedAt: Date.now(),
      isUnknown: !settled,
      consecutiveFailures: failures,
    }));
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
      isUnknown: true, // nothing has actually been observed yet
      consecutiveFailures: 0,
    }
  );
}
