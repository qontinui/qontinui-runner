/**
 * useApiFindings Hook
 *
 * Consumes the runner's persistent-storage findings REST surface (PR #255):
 *
 *   GET /findings?status=&category=&limit=&page=
 *   GET /findings/{id}
 *
 * This is the **cross-task-run** listing, distinct from the in-memory
 * `findingsTracker` (which is current-session-only) and from the per-task
 * `/findings/summary?task_run_id=…` endpoint that `useFindingsData`
 * (widgets/findings) consumes.
 *
 * Used by `ExecutionReport` to fall back to persistent findings when the
 * in-memory tracker is empty — so the Findings tab is no longer a permanent
 * "No Findings Yet" stub once data exists in `task_run_findings`.
 *
 * The API uses the canonical shared-types `Finding` shape directly, so no
 * field-by-field mapping is needed beyond optional-field defaults.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { Finding, FindingStatus, FindingSeverity } from "@/types/findings";
import { createLogger } from "@/lib/logger";

const logger = createLogger("useApiFindings");

/** Default polling interval (15s). Long because cross-run findings change slowly. */
const POLL_INTERVAL_MS = 15_000;

export interface UseApiFindingsOptions {
  /** Filter by status (server-side). */
  status?: FindingStatus | "wont_fix" | "deferred";
  /** Filter by category id (server-side). */
  category?: string;
  /** Page size (server clamps to 1..200). */
  limit?: number;
  /** 1-based page index. */
  page?: number;
  /** Poll the endpoint every `POLL_INTERVAL_MS`. Defaults to false. */
  poll?: boolean;
  /** Disable fetching entirely (e.g. while a parent is hidden). */
  enabled?: boolean;
}

export interface UseApiFindingsResult {
  /** Findings returned by the current query. */
  findings: Finding[];
  /** True between request fire and response. */
  isLoading: boolean;
  /** Non-null when the last fetch failed. */
  error: string | null;
  /** Current page index echoed back from the server. */
  page: number;
  /** Current page size echoed back from the server. */
  limit: number;
  /** Number of items on this page (≤ limit). */
  count: number;
  /** Force a refetch. */
  refresh: () => void;
}

interface ListFindingsResponse {
  items: Finding[];
  page: number;
  limit: number;
  count: number;
}

interface ApiEnvelope<T> {
  success?: boolean;
  data?: T;
  error?: string;
}

/**
 * Hook for fetching findings from the runner's REST findings surface.
 *
 * NOTE: callers using `severity` for client-side filtering should do so on the
 * returned `findings` array — the server's `/findings` endpoint does not yet
 * support a `severity` query param.
 */
export function useApiFindings(options: UseApiFindingsOptions = {}): UseApiFindingsResult {
  const {
    status,
    category,
    limit = 50,
    page = 1,
    poll = false,
    enabled = true,
  } = options;

  const [findings, setFindings] = useState<Finding[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [echoedPage, setEchoedPage] = useState(page);
  const [echoedLimit, setEchoedLimit] = useState(limit);
  const [count, setCount] = useState(0);
  const [refreshCounter, setRefreshCounter] = useState(0);

  // Keep an in-flight controller so we can cancel on unmount or refetch.
  const abortRef = useRef<AbortController | null>(null);

  const refresh = useCallback(() => {
    setRefreshCounter((c) => c + 1);
  }, []);

  useEffect(() => {
    if (!enabled) return;

    const params = new URLSearchParams();
    if (status) params.set("status", status);
    if (category) params.set("category", category);
    if (limit) params.set("limit", String(limit));
    if (page) params.set("page", String(page));

    const url = `${getApiBase()}/findings${params.toString() ? `?${params}` : ""}`;

    const controller = new AbortController();
    abortRef.current?.abort();
    abortRef.current = controller;

    let cancelled = false;
    setIsLoading(true);
    setError(null);

    (async () => {
      try {
        const response = await tracedFetch(url, { signal: controller.signal });
        if (!response.ok) {
          // 4xx/5xx — surface the error but do not throw; findings stays empty.
          const text = await response.text().catch(() => "");
          if (!cancelled) {
            setError(`HTTP ${response.status}${text ? `: ${text.slice(0, 200)}` : ""}`);
            setFindings([]);
          }
          return;
        }
        const body: ApiEnvelope<ListFindingsResponse> = await response.json();
        if (cancelled) return;
        if (body?.success === false) {
          setError(body.error ?? "API returned success=false");
          setFindings([]);
          return;
        }
        const list = body?.data;
        if (!list) {
          setFindings([]);
          return;
        }
        setFindings(Array.isArray(list.items) ? list.items : []);
        setEchoedPage(list.page ?? page);
        setEchoedLimit(list.limit ?? limit);
        setCount(list.count ?? list.items?.length ?? 0);
      } catch (err) {
        if (cancelled) return;
        if ((err as Error).name === "AbortError") return;
        logger.warn("useApiFindings fetch failed", err);
        setError(String((err as Error).message ?? err));
        setFindings([]);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
    // refreshCounter is included so consumers can force a refetch.
  }, [status, category, limit, page, enabled, refreshCounter]);

  // Optional polling. Driven by `refreshCounter` increment so we share the
  // same fetch effect above — keeps a single source of truth for fetch state.
  useEffect(() => {
    if (!enabled || !poll) return;
    const interval = setInterval(() => {
      setRefreshCounter((c) => c + 1);
    }, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [enabled, poll]);

  return {
    findings,
    isLoading,
    error,
    page: echoedPage,
    limit: echoedLimit,
    count,
    refresh,
  };
}

/**
 * Fetch a single finding by id (`GET /findings/{id}`).
 *
 * Lightweight one-shot fetch, no polling. Returns `null` while loading and
 * sets `error` on 404 or transport failure.
 */
export function useApiFinding(findingId: string | null): {
  finding: Finding | null;
  isLoading: boolean;
  error: string | null;
} {
  const [finding, setFinding] = useState<Finding | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!findingId) {
      setFinding(null);
      setError(null);
      return;
    }
    const controller = new AbortController();
    let cancelled = false;
    setIsLoading(true);
    setError(null);

    (async () => {
      try {
        const response = await tracedFetch(
          `${getApiBase()}/findings/${encodeURIComponent(findingId)}`,
          { signal: controller.signal },
        );
        if (response.status === 404) {
          if (!cancelled) {
            setError("not_found");
            setFinding(null);
          }
          return;
        }
        if (!response.ok) {
          if (!cancelled) {
            setError(`HTTP ${response.status}`);
            setFinding(null);
          }
          return;
        }
        const body: ApiEnvelope<Finding> = await response.json();
        if (cancelled) return;
        if (body?.success === false || !body?.data) {
          setError(body?.error ?? "API returned no data");
          setFinding(null);
          return;
        }
        setFinding(body.data);
      } catch (err) {
        if (cancelled) return;
        if ((err as Error).name === "AbortError") return;
        setError(String((err as Error).message ?? err));
        setFinding(null);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [findingId]);

  return { finding, isLoading, error };
}

// Re-exports kept narrow so callers can pin the param types without pulling
// the full shared-types module path.
export type { Finding, FindingStatus, FindingSeverity };
