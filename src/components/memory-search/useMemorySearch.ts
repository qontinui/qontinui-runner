import { useState, useCallback, useRef } from "react";
import type { MemoryResult, MemorySearchResponse, MemorySourceFilter } from "./types";

const API_BASE = "http://localhost:9876";

interface SearchParams {
  query: string;
  limit?: number;
  sources?: MemorySourceFilter[];
  from?: string;
  to?: string;
  minScore?: number;
}

interface SearchState {
  results: MemoryResult[];
  total: number;
  loading: boolean;
  error: string | null;
}

export function useMemorySearch() {
  const [state, setState] = useState<SearchState>({
    results: [],
    total: 0,
    loading: false,
    error: null,
  });
  const abortRef = useRef<AbortController | null>(null);

  const search = useCallback(async (params: SearchParams) => {
    // Cancel previous in-flight request
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;

    setState((prev) => ({ ...prev, loading: true, error: null }));

    const searchParams = new URLSearchParams();
    searchParams.set("q", params.query);
    if (params.limit) searchParams.set("limit", String(params.limit));
    if (params.sources?.length) searchParams.set("sources", params.sources.join(","));
    if (params.from) searchParams.set("from", params.from);
    if (params.to) searchParams.set("to", params.to);
    if (params.minScore !== undefined) searchParams.set("min_score", String(params.minScore));

    try {
      const res = await fetch(`${API_BASE}/memory/search?${searchParams}`, {
        signal: controller.signal,
      });
      const json: MemorySearchResponse = await res.json();
      if (json.success && json.data) {
        setState({
          results: json.data.results,
          total: json.data.total,
          loading: false,
          error: null,
        });
      } else {
        setState((prev) => ({ ...prev, loading: false, error: json.error || "Search failed" }));
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name === "AbortError") return;
      setState((prev) => ({
        ...prev,
        loading: false,
        error: err instanceof Error ? err.message : "Network error",
      }));
    }
  }, []);

  const clear = useCallback(() => {
    abortRef.current?.abort();
    setState({ results: [], total: 0, loading: false, error: null });
  }, []);

  return { ...state, search, clear };
}
