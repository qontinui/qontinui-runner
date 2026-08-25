/**
 * useErrorMonitor Hook
 *
 * React hook for managing error monitoring state and operations.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { errorMonitorService } from "../services/error-monitor-service";

/** Default refresh interval for error monitor polling (ms) */
export const ERROR_MONITOR_REFRESH_INTERVAL = 30_000;

/** Default query limit for error event fetches */
const DEFAULT_QUERY_LIMIT = 100;
import type {
  StoredErrorEvent,
  ErrorQuery,
  ErrorSummary,
  DebugContext,
  FixableErrorsSummary,
  ErrorSeverity,
  ErrorStatus,
} from "../types/errorMonitor";

// =============================================================================
// Error Events Hook
// =============================================================================

interface UseErrorEventsOptions {
  /** Task run ID to filter by */
  taskRunId?: string;
  /** Log source name to filter by */
  logSourceName?: string;
  /** Severities to include */
  severities?: ErrorSeverity[];
  /** Statuses to include */
  statuses?: ErrorStatus[];
  /** Maximum results */
  limit?: number;
  /** Auto-refresh interval in ms (0 to disable) */
  refreshInterval?: number;
}

/**
 * Value-identity of one server-side filter list, for a hook dependency list.
 *
 * `useErrorEvents`' callers build these arrays inline, so a fresh object every
 * render makes reference comparison useless in both directions: depend on the
 * reference and you refetch every render; depend on nothing (what the code did)
 * and you never refetch at all. A joined string compares by VALUE.
 *
 * `ErrorSeverity` and `ErrorStatus` are lowercase identifiers with no commas,
 * so join/split round-trips exactly. `undefined` and `[]` both mean "no filter"
 * to the query builder and both serialize to `""`, which is correct — swapping
 * one for the other must not trigger a refetch.
 */
export function filterListKey(values: readonly string[] | undefined): string {
  return values?.join(",") ?? "";
}

interface UseErrorEventsReturn {
  /** List of error events */
  errors: StoredErrorEvent[];
  /** Whether data is loading */
  loading: boolean;
  /** Error message if any */
  error: string | null;
  /** Refresh the error list */
  refresh: () => Promise<void>;
  /** Acknowledge an error */
  acknowledge: (id: number) => Promise<void>;
  /** Resolve an error */
  resolve: (id: number, notes?: string) => Promise<void>;
  /** Ignore an error */
  ignore: (id: number, reason?: string) => Promise<void>;
}

export function useErrorEvents(options: UseErrorEventsOptions = {}): UseErrorEventsReturn {
  const [errors, setErrors] = useState<StoredErrorEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Every filter the query is built from, as values `useCallback` can compare.
  //
  // WHY not `optionsRef` (manual-test-loop iter 19, item C): these filters are
  // applied by the SERVER, so changing one changes the result set — but
  // `fetchErrors` read them out of a ref and declared no dependencies, so its
  // identity never changed, no effect re-ran, and toggling a filter pill did
  // nothing visible until the 30s auto-refresh tick happened to come round.
  // The UI showed the new pill state over the old rows for up to half a minute.
  //
  // The arrays are compared as joined strings rather than by reference because
  // callers build them inline (`selectedStatuses.length > 0 ? selectedStatuses
  // : undefined`), and a fresh array every render would refetch every render.
  // `ErrorSeverity`/`ErrorStatus` are lowercase identifiers, so a comma is an
  // unambiguous separator.
  const { taskRunId, logSourceName, limit } = options;
  const severitiesKey = filterListKey(options.severities);
  const statusesKey = filterListKey(options.statuses);

  const fetchErrors = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const query: ErrorQuery = {
        taskRunId,
        logSourceName,
        severity: severitiesKey ? (severitiesKey.split(",") as ErrorSeverity[]) : undefined,
        status: statusesKey ? (statusesKey.split(",") as ErrorStatus[]) : undefined,
        limit: limit ?? DEFAULT_QUERY_LIMIT,
      };
      const result = await errorMonitorService.queryErrorEvents(query);
      setErrors(result);
    } catch (err) {
      const errorMessage =
        typeof err === "string"
          ? err
          : err instanceof Error
            ? err.message
            : "Failed to fetch errors";
      console.error("[useErrorMonitor] fetchErrors failed:", err);
      setError(errorMessage);
    } finally {
      setLoading(false);
    }
  }, [taskRunId, logSourceName, severitiesKey, statusesKey, limit]);

  // Initial fetch — and, since `fetchErrors` is now rebuilt whenever a filter
  // changes, the refetch that makes a pill toggle take effect immediately.
  // Deferred into a microtask so the setState inside
  // fetchErrors doesn't fire synchronously from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchErrors();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchErrors]);

  // Auto-refresh
  useEffect(() => {
    if (options.refreshInterval && options.refreshInterval > 0) {
      const interval = setInterval(fetchErrors, options.refreshInterval);
      return () => clearInterval(interval);
    }
  }, [options.refreshInterval, fetchErrors]);

  // Listen for new error events
  useEffect(() => {
    const unlisten = listen<StoredErrorEvent>("error-event-detected", () => {
      fetchErrors();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [fetchErrors]);

  const acknowledge = useCallback(async (id: number) => {
    await errorMonitorService.acknowledgeError(id);
    setErrors((prev) => prev.map((e) => (e.id === id ? { ...e, status: "acknowledged" } : e)));
  }, []);

  const resolve = useCallback(async (id: number, notes?: string) => {
    await errorMonitorService.resolveError(id, notes);
    setErrors((prev) =>
      prev.map((e) => (e.id === id ? { ...e, status: "resolved", resolution_notes: notes } : e)),
    );
  }, []);

  const ignore = useCallback(async (id: number, reason?: string) => {
    await errorMonitorService.ignoreError(id, reason);
    setErrors((prev) =>
      prev.map((e) => (e.id === id ? { ...e, status: "ignored", resolution_notes: reason } : e)),
    );
  }, []);

  return {
    errors,
    loading,
    error,
    refresh: fetchErrors,
    acknowledge,
    resolve,
    ignore,
  };
}

// =============================================================================
// Error Summary Hook
// =============================================================================

interface UseErrorSummaryOptions {
  /** Task run ID to filter by */
  taskRunId?: string;
  /** Auto-refresh interval in ms (0 to disable) */
  refreshInterval?: number;
}

interface UseErrorSummaryReturn {
  /** Error summary statistics */
  summary: ErrorSummary | null;
  /** Whether data is loading */
  loading: boolean;
  /** Error message if any */
  error: string | null;
  /** Refresh the summary */
  refresh: () => Promise<void>;
}

export function useErrorSummary(options: UseErrorSummaryOptions = {}): UseErrorSummaryReturn {
  const [summary, setSummary] = useState<ErrorSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const optionsRef = useRef(options);

  useEffect(() => {
    optionsRef.current = options;
  });

  const fetchSummary = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await errorMonitorService.getErrorSummary(optionsRef.current.taskRunId);
      setSummary(result);
    } catch (err) {
      const errorMessage =
        typeof err === "string"
          ? err
          : err instanceof Error
            ? err.message
            : "Failed to fetch summary";
      console.error("[useErrorMonitor] fetchSummary failed:", err);
      setError(errorMessage);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchSummary();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchSummary]);

  useEffect(() => {
    if (options.refreshInterval && options.refreshInterval > 0) {
      const interval = setInterval(fetchSummary, options.refreshInterval);
      return () => clearInterval(interval);
    }
  }, [options.refreshInterval, fetchSummary]);

  useEffect(() => {
    const unlisten = listen("error-event-detected", () => {
      fetchSummary();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [fetchSummary]);

  return { summary, loading, error, refresh: fetchSummary };
}

// Note: useLogSources hook has been removed.
// Log source management is now handled through global log source settings
// (Settings > Log Sources). See LogSourcesSettings.tsx and useGlobalLogSources.ts.

// =============================================================================
// Debug Context Hook
// =============================================================================

interface UseDebugContextOptions {
  /** Task run ID to filter by */
  taskRunId?: string;
  /** Maximum errors to include */
  maxErrors?: number;
}

interface UseDebugContextReturn {
  /** Curated debug context */
  context: DebugContext | null;
  /** Whether data is loading */
  loading: boolean;
  /** Error message if any */
  error: string | null;
  /** Refresh the context */
  refresh: () => Promise<void>;
  /** Get AI-formatted context string */
  getAiContext: () => Promise<string>;
}

export function useDebugContext(options: UseDebugContextOptions = {}): UseDebugContextReturn {
  const [context, setContext] = useState<DebugContext | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const optionsRef = useRef(options);

  useEffect(() => {
    optionsRef.current = options;
  });

  const fetchContext = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await errorMonitorService.getDebugContext(
        optionsRef.current.taskRunId,
        optionsRef.current.maxErrors,
      );
      setContext(result);
    } catch (err) {
      const errorMessage =
        typeof err === "string"
          ? err
          : err instanceof Error
            ? err.message
            : "Failed to fetch debug context";
      console.error("[useErrorMonitor] fetchContext failed:", err);
      setError(errorMessage);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchContext();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchContext]);

  const getAiContext = useCallback(async () => {
    return errorMonitorService.getDebugContextForAi(optionsRef.current.taskRunId);
  }, []);

  return { context, loading, error, refresh: fetchContext, getAiContext };
}

// =============================================================================
// Fix Workflow Hook
// =============================================================================

interface UseFixWorkflowReturn {
  /** Fixable errors summary */
  summary: FixableErrorsSummary | null;
  /** Whether data is loading */
  loading: boolean;
  /** Error message if any */
  error: string | null;
  /** Check for fixable errors */
  check: (taskRunId?: string) => Promise<FixableErrorsSummary>;
  /** Generate fix workflow */
  generateWorkflow: (taskRunId?: string) => Promise<Record<string, unknown>>;
  /** Generate workflow for single error */
  generateSingleErrorWorkflow: (errorId: number) => Promise<Record<string, unknown>>;
}

export function useFixWorkflow(): UseFixWorkflowReturn {
  const [summary, setSummary] = useState<FixableErrorsSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const check = useCallback(async (taskRunId?: string) => {
    try {
      setLoading(true);
      setError(null);
      const result = await errorMonitorService.checkFixableErrors(taskRunId);
      setSummary(result);
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to check fixable errors";
      setError(msg);
      throw new Error(msg, { cause: err });
    } finally {
      setLoading(false);
    }
  }, []);

  const generateWorkflow = useCallback(async (taskRunId?: string) => {
    try {
      setLoading(true);
      setError(null);
      const config = taskRunId ? { taskRunId } : undefined;
      const result = await errorMonitorService.generateErrorFixWorkflow(config);
      return result.workflowJson;
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to generate workflow";
      setError(msg);
      throw new Error(msg, { cause: err });
    } finally {
      setLoading(false);
    }
  }, []);

  const generateSingleErrorWorkflow = useCallback(async (errorId: number) => {
    try {
      setLoading(true);
      setError(null);
      const result = await errorMonitorService.generateSingleErrorFixWorkflow(errorId);
      return result.workflowJson;
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to generate workflow";
      setError(msg);
      throw new Error(msg, { cause: err });
    } finally {
      setLoading(false);
    }
  }, []);

  return {
    summary,
    loading,
    error,
    check,
    generateWorkflow,
    generateSingleErrorWorkflow,
  };
}

// =============================================================================
// Error Badge Hook (for status indicator)
// =============================================================================

interface UseErrorBadgeReturn {
  /** Number of unresolved errors */
  count: number;
  /** Whether there are critical/error severity issues */
  hasActionable: boolean;
  /** Highest severity among unresolved */
  highestSeverity: ErrorSeverity | null;
}

export function useErrorBadge(taskRunId?: string): UseErrorBadgeReturn {
  const [count, setCount] = useState(0);
  const [hasActionable, setHasActionable] = useState(false);
  const [highestSeverity, setHighestSeverity] = useState<ErrorSeverity | null>(null);

  useEffect(() => {
    const fetchBadgeData = async () => {
      try {
        const summary = await errorMonitorService.getErrorSummary(taskRunId);
        setCount(summary.unresolvedCount);
        setHasActionable(summary.hasActionableErrors);

        // Determine highest severity
        if (summary.criticalCount > 0) {
          setHighestSeverity("critical");
        } else if (summary.errorCount > 0) {
          setHighestSeverity("error");
        } else if (summary.warningCount > 0) {
          setHighestSeverity("warning");
        } else {
          setHighestSeverity(null);
        }
      } catch {
        // Silently fail for badge
      }
    };

    fetchBadgeData();

    // Refresh every 30 seconds
    const interval = setInterval(fetchBadgeData, ERROR_MONITOR_REFRESH_INTERVAL);

    // Listen for new errors
    const unlisten = listen("error-event-detected", fetchBadgeData);

    return () => {
      clearInterval(interval);
      unlisten.then((fn) => fn());
    };
  }, [taskRunId]);

  return { count, hasActionable, highestSeverity };
}
