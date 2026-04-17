/**
 * useSearchEvents.ts
 *
 * React Query hook that calls the Tauri `search_events` command to run
 * a full-text search across activity_timeline, observations,
 * deferred_questions, and error_events (defaults: last 7 days, cap 500).
 *
 * The query input is debounced by 300ms before firing. Queries shorter
 * than 2 characters (after trimming) are disabled.
 */

import { useEffect, useState } from "react";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

// =============================================================================
// Types
// =============================================================================

export type SearchEventSourceTable =
  | "activity_timeline"
  | "observations"
  | "deferred_questions"
  | "error_events";

export interface SearchEventResult {
  source_table: SearchEventSourceTable;
  record_id: string;
  snippet: string;
  ts: string;
  score: number;
}

export interface UseSearchEventsParams {
  query: string;
  sinceIso?: string;
  limit?: number;
  enabled?: boolean;
}

// =============================================================================
// Debounce helper
// =============================================================================

/** Minimal inline debounce — no shared util in this codebase. */
function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);

  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(t);
  }, [value, delayMs]);

  return debounced;
}

// =============================================================================
// Hook
// =============================================================================

const MIN_QUERY_LENGTH = 2;
const DEBOUNCE_MS = 300;

export function useSearchEvents(
  params: UseSearchEventsParams,
): UseQueryResult<SearchEventResult[], Error> {
  const { query, sinceIso, limit, enabled = true } = params;

  const debouncedQuery = useDebouncedValue(query, DEBOUNCE_MS);
  const trimmed = debouncedQuery.trim();
  const isQueryValid = trimmed.length >= MIN_QUERY_LENGTH;

  return useQuery<SearchEventResult[], Error>({
    queryKey: ["search-events", trimmed, sinceIso ?? null, limit ?? null],
    queryFn: async () => {
      try {
        const args: { query: string; since_iso?: string; limit?: number } = {
          query: trimmed,
        };
        if (sinceIso !== undefined) args.since_iso = sinceIso;
        if (limit !== undefined) args.limit = limit;

        const result = await invoke<SearchEventResult[]>("search_events", args);
        return result;
      } catch (err) {
        // Tauri commands reject with a string; normalize to Error.
        if (err instanceof Error) throw err;
        if (typeof err === "string") throw new Error(err, { cause: err });
        throw new Error(
          typeof err === "object" && err !== null ? JSON.stringify(err) : "search_events failed",
          { cause: err },
        );
      }
    },
    enabled: enabled && isQueryValid,
    staleTime: 15_000,
  });
}
