/**
 * useBrowserErrors Hook
 *
 * Fetches browser-side error data from the UI Bridge SDK proxy endpoints.
 * Combines health status, browser events, and error snapshots into a
 * single report that auto-refreshes on an interval.
 *
 * Endpoints used (via the runner backend proxy):
 *   GET /ui-bridge/sdk/console/health
 *   GET /ui-bridge/sdk/console/browser-events?severity=error&deduplicate=true
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type {
  BrowserHealthReport,
  BrowserCapturedEvent,
  BrowserEventsResponse,
  BrowserErrorReport,
  FingerprintedEvent,
} from "../types/browserErrors";

// =============================================================================
// Types
// =============================================================================

interface UseBrowserErrorsOptions {
  /** Auto-refresh interval in ms (0 to disable). Default: 30000 */
  refreshInterval?: number;
  /** Whether the hook is enabled. Default: true */
  enabled?: boolean;
}

interface UseBrowserErrorsReturn {
  /** Composite report (health + events + snapshots) */
  report: BrowserErrorReport | null;
  /** Health report only */
  health: BrowserHealthReport | null;
  /** Recent browser errors */
  recentErrors: BrowserCapturedEvent[];
  /** Deduplicated error groups */
  deduplicatedErrors: FingerprintedEvent[];
  /** Whether data is currently loading */
  loading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Whether no SDK connection is available */
  noConnection: boolean;
  /** Manually refresh data */
  refresh: () => Promise<void>;
}

// =============================================================================
// Hook
// =============================================================================

export function useBrowserErrors(options: UseBrowserErrorsOptions = {}): UseBrowserErrorsReturn {
  const { refreshInterval = 30000, enabled = true } = options;

  const [report, setReport] = useState<BrowserErrorReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [noConnection, setNoConnection] = useState(false);

  const enabledRef = useRef(enabled);
  // eslint-disable-next-line react-hooks/refs
  enabledRef.current = enabled;

  const fetchReport = useCallback(async () => {
    if (!enabledRef.current) return;

    try {
      setLoading(true);
      setError(null);

      const base = getApiBase();

      // Fetch health and browser events in parallel
      const [healthResp, eventsResp] = await Promise.all([
        tracedFetch(`${base}/ui-bridge/sdk/console/health`),
        tracedFetch(`${base}/ui-bridge/sdk/console/browser-events?severity=error&deduplicate=true`),
      ]);

      // Parse responses
      const healthJson = await healthResp.json();
      const eventsJson = await eventsResp.json();

      // Check for connection errors (SDK not connected)
      if (
        healthJson.success === false &&
        typeof healthJson.error === "string" &&
        (healthJson.error.includes("not connected") ||
          healthJson.error.includes("No SDK") ||
          healthJson.error.includes("no active"))
      ) {
        setNoConnection(true);
        setReport(null);
        return;
      }

      setNoConnection(false);

      // Extract data from the response wrappers.
      // The proxy returns the SDK response directly; the SDK wraps data in
      // { success: true, data: ... } or returns the payload at the top level.
      const healthData: BrowserHealthReport = healthJson.data ?? healthJson;
      const eventsData: BrowserEventsResponse = eventsJson.data ?? eventsJson;

      const compositeReport: BrowserErrorReport = {
        health: healthData,
        recentErrors: eventsData.events ?? [],
        snapshots: [],
      };

      setReport(compositeReport);
    } catch (err) {
      const message = err instanceof Error ? err.message : "Failed to fetch browser errors";

      // Network errors likely mean SDK is not connected
      if (
        message.includes("Failed to fetch") ||
        message.includes("NetworkError") ||
        message.includes("ECONNREFUSED")
      ) {
        setNoConnection(true);
        setReport(null);
      } else {
        setError(message);
      }

      console.error("[useBrowserErrors] fetch failed:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial fetch
  useEffect(() => {
    if (enabled) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      fetchReport();
    }
  }, [enabled, fetchReport]);

  // Auto-refresh
  useEffect(() => {
    if (!enabled || !refreshInterval || refreshInterval <= 0) return;

    const interval = setInterval(fetchReport, refreshInterval);
    return () => clearInterval(interval);
  }, [enabled, refreshInterval, fetchReport]);

  // Derived data
  const health = report?.health ?? null;
  const recentErrors = report?.recentErrors ?? [];

  // We need to store deduplicated separately since our composite report
  // does not include it directly.
  const [deduplicatedErrors, setDeduplicatedErrors] = useState<FingerprintedEvent[]>([]);

  // Update deduplicated errors when report changes
  useEffect(() => {
    if (!report) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDeduplicatedErrors([]);
      return;
    }

    // The report.recentErrors came from the browser-events call which also
    // returns deduplicated groups in the same response. Since we cannot store
    // those inside the composite report cleanly, we re-derive them: group by
    // type+message as a lightweight fingerprint.
    const groups = new Map<string, FingerprintedEvent>();
    for (const event of report.recentErrors) {
      const key = `${event.type}:${event.message ?? event.errorMessage ?? event.requestUrl ?? "unknown"}`;
      const existing = groups.get(key);
      if (existing) {
        existing.count += 1;
        existing.lastSeen = Math.max(existing.lastSeen, event.timestamp);
        existing.firstSeen = Math.min(existing.firstSeen, event.timestamp);
      } else {
        groups.set(key, {
          fingerprint: key,
          count: 1,
          firstSeen: event.timestamp,
          lastSeen: event.timestamp,
          event,
        });
      }
    }
    setDeduplicatedErrors(Array.from(groups.values()).sort((a, b) => b.lastSeen - a.lastSeen));
  }, [report]);

  return {
    report,
    health,
    recentErrors,
    deduplicatedErrors,
    loading,
    error,
    noConnection,
    refresh: fetchReport,
  };
}
