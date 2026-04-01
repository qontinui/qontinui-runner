/**
 * React hooks for the Knowledge Acquisition system.
 *
 * Provides access to search, stats, and provider information
 * via the /knowledge/* API endpoints.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getApiBase } from "@/lib/runner-api";
import type {
  StatsSnapshot,
  ProviderInfo,
  SearchResponse,
  VulnAuditResponse,
} from "@/services/knowledge-acquisition-service";

// ============================================================================
// Query Key Factory
// ============================================================================

export const knowledgeKeys = {
  all: ["knowledge"] as const,
  stats: () => [...knowledgeKeys.all, "stats"] as const,
  providers: () => [...knowledgeKeys.all, "providers"] as const,
  search: (query: string) => [...knowledgeKeys.all, "search", query] as const,
  vulnSearch: (query: string) => [...knowledgeKeys.all, "vuln-search", query] as const,
};

// ============================================================================
// Fetch Helper
// ============================================================================

async function fetchKnowledge<T>(path: string, body?: unknown): Promise<T> {
  const url = `${getApiBase()}${path}`;
  const res = await fetch(url, {
    method: body ? "POST" : "GET",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`Knowledge API ${res.status}`);
  const json = await res.json();
  if (!json.success) throw new Error(json.error || "Unknown error");
  return json.data as T;
}

// ============================================================================
// Hooks
// ============================================================================

/** Fetch knowledge acquisition stats (auto-refreshes every 30s) */
export function useKnowledgeStats() {
  return useQuery({
    queryKey: knowledgeKeys.stats(),
    queryFn: () => fetchKnowledge<StatsSnapshot>("/knowledge/stats"),
    staleTime: 10_000,
    refetchInterval: 30_000,
    retry: 1,
  });
}

/** Fetch available providers */
export function useKnowledgeProviders() {
  return useQuery({
    queryKey: knowledgeKeys.providers(),
    queryFn: () => fetchKnowledge<ProviderInfo[]>("/knowledge/providers"),
    staleTime: 60_000,
  });
}

/** Perform a web search (manual trigger via mutation) */
export function useKnowledgeSearch() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ query, maxResults = 5 }: { query: string; maxResults?: number }) =>
      fetchKnowledge<SearchResponse>("/knowledge/search-web", {
        query,
        max_results: maxResults,
      }),
    onSuccess: () => {
      // Invalidate stats since a search was performed
      queryClient.invalidateQueries({ queryKey: knowledgeKeys.stats() });
    },
  });
}

/** Research an error message */
export function useResearchError() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      errorMessage,
      framework,
      stackTrace,
    }: {
      errorMessage: string;
      framework?: string;
      stackTrace?: string;
    }) =>
      fetchKnowledge<SearchResponse>("/knowledge/research-error", {
        error_message: errorMessage,
        framework,
        stack_trace: stackTrace,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: knowledgeKeys.stats() });
    },
  });
}

/** Search for vulnerabilities */
export function useVulnerabilitySearch() {
  return useMutation({
    mutationFn: async (query: string) =>
      fetchKnowledge<SearchResponse>("/knowledge/search-vulnerabilities", {
        query,
      }),
  });
}

/** Audit dependencies from a package list */
export function useAuditDependencies() {
  return useMutation({
    mutationFn: async (packages: Array<{ name: string; ecosystem: string; version: string }>) =>
      fetchKnowledge<VulnAuditResponse>("/knowledge/audit-dependencies", {
        packages,
      }),
  });
}
