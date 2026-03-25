/**
 * usePromptVersions — TanStack Query hooks for prompt template version history.
 *
 * Provides fetching and mutation hooks for the prompt version history API.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface PromptVersion {
  version_number: number;
  content: string;
  change_description: string;
  content_hash: string;
  created_at: string;
  performance_metrics?: {
    success_rate?: number;
    avg_cost?: number;
    avg_latency?: number;
  } | null;
}

export interface PromptVersionDiff {
  version_a: number;
  version_b: number;
  unified_diff: string;
  additions: number;
  deletions: number;
}

export interface CreateVersionRequest {
  content: string;
  change_description: string;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const promptVersionKeys = {
  all: ["promptVersions"] as const,
  list: (templateId: string) => [...promptVersionKeys.all, "list", templateId] as const,
  detail: (templateId: string, versionNumber: number) =>
    [...promptVersionKeys.all, "detail", templateId, versionNumber] as const,
  diff: (templateId: string, versionA: number, versionB: number) =>
    [...promptVersionKeys.all, "diff", templateId, versionA, versionB] as const,
};

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

const API_PREFIX = "/api/v1/ai-prompt-templates";

async function fetchJson<T>(url: string): Promise<T> {
  const response = await tracedFetch(`${getApiBase()}${url}`);
  if (!response.ok) {
    throw new Error(`API error ${response.status}: ${response.statusText}`);
  }
  const json = await response.json();
  if (json.data !== undefined) return json.data as T;
  return json as T;
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/**
 * Fetch the version list for a prompt template.
 */
export function usePromptVersions(templateId: string | null) {
  return useQuery({
    queryKey: promptVersionKeys.list(templateId ?? ""),
    queryFn: () => fetchJson<PromptVersion[]>(`${API_PREFIX}/${templateId}/versions`),
    enabled: !!templateId,
    staleTime: 10_000,
  });
}

/**
 * Fetch a single version of a prompt template.
 */
export function usePromptVersion(templateId: string | null, versionNumber: number | null) {
  return useQuery({
    queryKey: promptVersionKeys.detail(templateId ?? "", versionNumber ?? 0),
    queryFn: () =>
      fetchJson<PromptVersion>(`${API_PREFIX}/${templateId}/versions/${versionNumber}`),
    enabled: !!templateId && versionNumber != null,
    staleTime: 60_000, // versions are immutable
  });
}

/**
 * Fetch a unified diff between two versions.
 */
export function usePromptVersionDiff(
  templateId: string | null,
  versionA: number | null,
  versionB: number | null,
) {
  return useQuery({
    queryKey: promptVersionKeys.diff(templateId ?? "", versionA ?? 0, versionB ?? 0),
    queryFn: () =>
      fetchJson<PromptVersionDiff>(
        `${API_PREFIX}/${templateId}/versions/${versionA}/diff/${versionB}`,
      ),
    enabled: !!templateId && versionA != null && versionB != null,
    staleTime: 60_000,
  });
}

/**
 * Mutation: create a new version for a prompt template.
 */
export function useCreatePromptVersion(templateId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (req: CreateVersionRequest) => {
      const response = await tracedFetch(`${getApiBase()}${API_PREFIX}/${templateId}/versions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(req),
      });
      if (!response.ok) {
        throw new Error(`API error ${response.status}: ${response.statusText}`);
      }
      return response.json();
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: promptVersionKeys.list(templateId) });
    },
  });
}
