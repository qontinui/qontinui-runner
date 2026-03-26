/**
 * useObservationMemory.ts
 *
 * Hook for fetching and injecting observation-based memory context into AI tasks.
 * Queries the /observations/context endpoint for project-relevant observations
 * and formats them as injectable context strings (Engram-inspired progressive disclosure).
 */

import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ============================================================================
// Types
// ============================================================================

export interface ObservationSearchResult {
  id: number;
  title: string;
  contentPreview: string;
  observationType: string;
  scope: string;
  topicKey?: string | null;
  revisionCount: number;
  projectId?: string | null;
  createdAt: string;
  updatedAt: string;
  rank?: number | null;
}

export interface ObservationMemoryContext {
  /** Formatted markdown string ready for prompt injection */
  contextContent: string;
  /** Number of observations included */
  observationCount: number;
  /** Individual observations for UI display */
  observations: ObservationSearchResult[];
  /** Whether memory context is loading */
  loading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Whether PG/observations are available */
  available: boolean;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// ============================================================================
// Constants
// ============================================================================

const OBSERVATION_TYPE_LABELS: Record<string, string> = {
  decision: "Decision",
  architecture: "Architecture",
  bugfix: "Bug Fix",
  pattern: "Pattern",
  learning: "Learning",
  discovery: "Discovery",
};

// ============================================================================
// Hook
// ============================================================================

/**
 * Fetch project-relevant observations and format as injectable AI context.
 *
 * Uses progressive disclosure: previews are included in the context string,
 * with observation IDs for the AI to request full content via mem_get.
 */
export function useObservationMemory(
  projectId: string | null,
  options: {
    /** Max observations to include (default 15) */
    maxResults?: number;
    /** Filter by type (default: all types) */
    observationType?: string;
    /** Auto-refresh interval in ms (0 = disabled, default 0) */
    refreshInterval?: number;
    /** Whether to enable the hook (default true) */
    enabled?: boolean;
  } = {},
): ObservationMemoryContext {
  const {
    maxResults = 15,
    observationType,
    refreshInterval = 0,
    enabled = true,
  } = options;

  const [observations, setObservations] = useState<ObservationSearchResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [available, setAvailable] = useState(true);
  const refreshTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchContext = useCallback(async () => {
    if (!projectId || !enabled) {
      setObservations([]);
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const params = new URLSearchParams({
        project_id: projectId,
        max_results: String(maxResults),
      });
      if (observationType) {
        params.set("observation_type", observationType);
      }

      const response = await tracedFetch(
        `${getApiBase()}/observations/context?${params}`,
      );

      if (response.status === 503) {
        // PG not available
        setAvailable(false);
        setObservations([]);
        return;
      }

      const result: ApiResponse<ObservationSearchResult[]> = await response.json();

      if (result.success && result.data) {
        setAvailable(true);
        setObservations(result.data);
      } else {
        setError(result.error || "Failed to fetch observations");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch memory context");
    } finally {
      setLoading(false);
    }
  }, [projectId, maxResults, observationType, enabled]);

  // Initial fetch
  useEffect(() => {
    fetchContext();
  }, [fetchContext]);

  // Optional auto-refresh
  useEffect(() => {
    if (!refreshInterval || refreshInterval <= 0) return;

    refreshTimerRef.current = setInterval(fetchContext, refreshInterval);
    return () => {
      if (refreshTimerRef.current) {
        clearInterval(refreshTimerRef.current);
      }
    };
  }, [refreshInterval, fetchContext]);

  // Format observations as markdown context for prompt injection
  const contextContent = useMemo(
    () => formatObservationsAsContext(observations),
    [observations],
  );

  return {
    contextContent,
    observationCount: observations.length,
    observations,
    loading,
    error,
    available,
  };
}

// ============================================================================
// Search hook (for explicit queries, not project context)
// ============================================================================

export function useObservationSearch() {
  const [results, setResults] = useState<ObservationSearchResult[]>([]);
  const [loading, setLoading] = useState(false);

  const search = useCallback(
    async (query: string, projectId?: string, maxResults = 20) => {
      if (!query.trim()) {
        setResults([]);
        return;
      }

      setLoading(true);
      try {
        const params = new URLSearchParams({
          q: query,
          max_results: String(maxResults),
        });
        if (projectId) {
          params.set("project_id", projectId);
        }

        const response = await tracedFetch(
          `${getApiBase()}/observations/search?${params}`,
        );
        const result: ApiResponse<ObservationSearchResult[]> = await response.json();

        if (result.success && result.data) {
          setResults(result.data);
        }
      } catch (err) {
        console.error("[ObservationSearch] Error:", err);
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  return { results, loading, search };
}

// ============================================================================
// Formatting
// ============================================================================

/**
 * Format observations as a markdown context block for AI prompt injection.
 *
 * Uses progressive disclosure: 300-char previews with IDs.
 * The AI agent can request full content via GET /observations/:id.
 */
function formatObservationsAsContext(
  observations: ObservationSearchResult[],
): string {
  if (observations.length === 0) return "";

  const lines: string[] = [
    "## Memory Context (from past sessions)",
    "",
    `${observations.length} relevant observation(s) from previous work.`,
    "Use GET /observations/:id for full content of any observation.",
    "",
  ];

  // Group by type
  const byType = new Map<string, ObservationSearchResult[]>();
  for (const obs of observations) {
    const group = byType.get(obs.observationType) || [];
    group.push(obs);
    byType.set(obs.observationType, group);
  }

  for (const [type, items] of byType) {
    const label = OBSERVATION_TYPE_LABELS[type] || type;
    lines.push(`### ${label}s`);
    lines.push("");

    for (const obs of items) {
      const rev =
        obs.revisionCount > 1 ? ` (rev ${obs.revisionCount})` : "";
      const topicTag = obs.topicKey ? ` [${obs.topicKey}]` : "";
      lines.push(`- **${obs.title}**${rev}${topicTag} (id: ${obs.id})`);
      lines.push(`  ${obs.contentPreview}`);
      lines.push("");
    }
  }

  return lines.join("\n");
}
