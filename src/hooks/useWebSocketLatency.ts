/**
 * useWebSocketLatency Hook
 *
 * Tracks WebSocket message latency and connection health.
 * Measures time from backend emit to frontend receive.
 *
 * Integrates with the central performance monitor.
 * Only active in development mode.
 */

import { useCallback, useRef, useState } from "react";
import { getPerformanceMonitor } from "../lib/performance";

// ============================================================================
// Types
// ============================================================================

export interface WebSocketLatencyStats {
  /** Total messages received */
  messagesReceived: number;
  /** Average latency in ms (if timestamps available) */
  averageLatency: number;
  /** Last message latency in ms */
  lastLatency: number;
  /** Number of reconnection attempts */
  reconnectCount: number;
  /** Whether currently connected */
  isConnected: boolean;
  /** Messages per minute rate */
  messageRate: number;
}

export interface UseWebSocketLatencyOptions {
  /** Whether tracking is enabled (defaults to DEV mode) */
  enabled?: boolean;
}

export interface UseWebSocketLatencyReturn {
  /** Current latency stats */
  stats: WebSocketLatencyStats;
  /** Record a received message with optional server timestamp */
  recordMessage: (serverTimestamp?: number) => void;
  /** Record connection state change */
  setConnected: (connected: boolean) => void;
  /** Calculate latency from server timestamp */
  calculateLatency: (serverTimestamp: number) => number;
  /** Reset all stats */
  reset: () => void;
}

// ============================================================================
// Hook Implementation
// ============================================================================

/**
 * Hook for tracking WebSocket performance metrics.
 *
 * @example
 * ```tsx
 * function MyWebSocketComponent() {
 *   const { stats, recordMessage, setConnected } = useWebSocketLatency();
 *
 *   useEffect(() => {
 *     const ws = new WebSocket(url);
 *
 *     ws.onopen = () => setConnected(true);
 *     ws.onclose = () => setConnected(false);
 *     ws.onmessage = (event) => {
 *       const data = JSON.parse(event.data);
 *       recordMessage(data.timestamp); // If server includes timestamp
 *     };
 *
 *     return () => ws.close();
 *   }, []);
 *
 *   return <div>WS Latency: {stats.averageLatency.toFixed(1)}ms</div>;
 * }
 * ```
 */
export function useWebSocketLatency(
  options: UseWebSocketLatencyOptions = {}
): UseWebSocketLatencyReturn {
  const { enabled = import.meta.env.DEV } = options;

  const [stats, setStats] = useState<WebSocketLatencyStats>({
    messagesReceived: 0,
    averageLatency: 0,
    lastLatency: 0,
    reconnectCount: 0,
    isConnected: false,
    messageRate: 0,
  });

  const messagesRef = useRef(0);
  const latenciesRef = useRef<number[]>([]);
  const messageTimesRef = useRef<number[]>([]);
  const reconnectsRef = useRef(0);
  const connectedRef = useRef(false);
  const startTimeRef = useRef(Date.now());

  // Calculate latency from server timestamp
  const calculateLatency = useCallback((serverTimestamp: number): number => {
    // Server timestamp should be in milliseconds
    // Handle both Unix timestamps and ISO strings
    let serverTime: number;

    if (typeof serverTimestamp === "number") {
      // If it looks like seconds, convert to ms
      if (serverTimestamp < 10000000000) {
        serverTime = serverTimestamp * 1000;
      } else {
        serverTime = serverTimestamp;
      }
    } else {
      serverTime = new Date(serverTimestamp).getTime();
    }

    const now = Date.now();
    const latency = now - serverTime;

    // Sanity check - negative or very large latencies indicate clock skew
    if (latency < 0 || latency > 60000) {
      return 0;
    }

    return latency;
  }, []);

  // Record a received message
  const recordMessage = useCallback(
    (serverTimestamp?: number) => {
      if (!enabled) return;

      messagesRef.current++;
      messageTimesRef.current.push(Date.now());

      // Keep only last minute of message times
      const oneMinuteAgo = Date.now() - 60000;
      messageTimesRef.current = messageTimesRef.current.filter((t) => t > oneMinuteAgo);

      let latency: number | undefined;
      if (serverTimestamp !== undefined) {
        latency = calculateLatency(serverTimestamp);
        if (latency > 0) {
          latenciesRef.current.push(latency);
          // Keep only last 100 latencies
          if (latenciesRef.current.length > 100) {
            latenciesRef.current = latenciesRef.current.slice(-50);
          }
        }
      }

      // Record in performance monitor
      getPerformanceMonitor().recordWebSocketMessage(latency);

      // Update stats
      const avgLatency =
        latenciesRef.current.length > 0
          ? latenciesRef.current.reduce((a, b) => a + b, 0) / latenciesRef.current.length
          : 0;

      const uptime = Date.now() - startTimeRef.current;
      const messageRate = uptime > 0 ? (messagesRef.current / (uptime / 60000)) : 0;

      setStats({
        messagesReceived: messagesRef.current,
        averageLatency: avgLatency,
        lastLatency: latency ?? 0,
        reconnectCount: reconnectsRef.current,
        isConnected: connectedRef.current,
        messageRate,
      });
    },
    [enabled, calculateLatency]
  );

  // Record connection state change
  const setConnected = useCallback(
    (connected: boolean) => {
      if (!enabled) return;

      if (!connected && connectedRef.current) {
        // Was connected, now disconnected - increment reconnect count
        reconnectsRef.current++;
      }

      connectedRef.current = connected;
      getPerformanceMonitor().setWebSocketConnected(connected);

      setStats((prev) => ({
        ...prev,
        isConnected: connected,
        reconnectCount: reconnectsRef.current,
      }));
    },
    [enabled]
  );

  // Reset all stats
  const reset = useCallback(() => {
    messagesRef.current = 0;
    latenciesRef.current = [];
    messageTimesRef.current = [];
    reconnectsRef.current = 0;
    startTimeRef.current = Date.now();

    setStats({
      messagesReceived: 0,
      averageLatency: 0,
      lastLatency: 0,
      reconnectCount: 0,
      isConnected: connectedRef.current,
      messageRate: 0,
    });
  }, []);

  return {
    stats,
    recordMessage,
    setConnected,
    calculateLatency,
    reset,
  };
}

// ============================================================================
// Polling vs WebSocket Comparison
// ============================================================================

export interface PollingComparison {
  /** Estimated requests saved per minute */
  requestsSavedPerMinute: number;
  /** Estimated latency improvement (polling avg - ws avg) */
  latencyImprovement: number;
  /** Whether WebSocket is more efficient */
  webSocketBetter: boolean;
  /** Explanation of comparison */
  explanation: string;
}

/**
 * Compare WebSocket performance to estimated polling performance.
 *
 * @param wsStats - WebSocket latency stats
 * @param pollingIntervalMs - How often polling would occur
 * @param pollingLatencyMs - Average polling request latency
 */
export function compareToPolling(
  wsStats: WebSocketLatencyStats,
  pollingIntervalMs: number = 5000,
  pollingLatencyMs: number = 100
): PollingComparison {
  // Calculate polling requests per minute
  const pollingRequestsPerMinute = 60000 / pollingIntervalMs;

  // WebSocket only needs 1 persistent connection vs N polling requests
  const requestsSaved = pollingRequestsPerMinute - 1;

  // Latency: WebSocket delivers immediately, polling has interval delay + request latency
  // Average polling delay = interval/2 + request latency
  const avgPollingLatency = pollingIntervalMs / 2 + pollingLatencyMs;
  const latencyImprovement = avgPollingLatency - wsStats.averageLatency;

  const webSocketBetter = requestsSaved > 0 && latencyImprovement > 0;

  let explanation: string;
  if (webSocketBetter) {
    explanation = `WebSocket saves ~${Math.round(requestsSaved)} req/min and delivers ${Math.round(latencyImprovement)}ms faster on average.`;
  } else if (requestsSaved > 0) {
    explanation = `WebSocket saves ~${Math.round(requestsSaved)} req/min but has ${Math.round(-latencyImprovement)}ms higher latency.`;
  } else {
    explanation = `Polling at ${pollingIntervalMs}ms is comparable to WebSocket for this use case.`;
  }

  return {
    requestsSavedPerMinute: Math.max(0, requestsSaved),
    latencyImprovement,
    webSocketBetter,
    explanation,
  };
}

export default useWebSocketLatency;
