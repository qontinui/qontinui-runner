/**
 * useOneshotStats
 *
 * Fetch process-local counters for `OneshotLlm` adapter calls (Phase 0 of
 * `plans/productivity-stack-product-readiness.md`). Backs the sibling tile
 * on the {@link ScriptedOutputPanel} that surfaces one-shot
 * productivity-task LLM activity (decompose-plan, auto-review,
 * rewind-session, summarize-session).
 *
 * Counters reset on runner restart — same lifecycle as the scripted-output
 * Phase A counters. Persistent aggregation is a follow-up if Phases 3–5
 * reveal demand.
 *
 * Source: `get_oneshot_stats` Tauri command at
 * `qontinui-runner/src-tauri/src/commands/activity_timeline.rs`.
 */

import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

/**
 * Shape returned by the `get_oneshot_stats` Tauri command. Stable wire keys
 * — Rust side uses `#[serde(rename_all = "camelCase")]`.
 */
export interface OneshotStats {
  /** Successful calls per adapter (`claude_api_warm`, `claude_api`, `gemini_api`, `local`). */
  callsTotal: Record<string, number>;
  /** Error count per `kind` (`transport`, `no_credentials`, `disabled`, `schema_parse`, `join_error`). */
  errorsTotal: Record<string, number>;
  /** Calls per provider whose response carried `cache_read_tokens > 0`. */
  cacheHitsTotal: Record<string, number>;
  /** Sum of `cache_read_tokens` across all successful calls. */
  cacheReadTokens: number;
  /** Sum of `cache_creation_tokens` across all successful calls. */
  cacheCreationTokens: number;
  /** Sum of `input_tokens` across all successful calls. */
  totalTokensIn: number;
  /** Sum of `output_tokens` across all successful calls. */
  totalTokensOut: number;
}

export const oneshotKeys = {
  all: ["oneshot-stats"] as const,
};

export interface UseOneshotStatsResult {
  stats: OneshotStats | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Fetch process-local counters for `OneshotLlm` adapter calls.
 *
 * Returns null while loading. Empty counters render as the "no one-shot
 * activity yet" state in the tile.
 */
export function useOneshotStats(): UseOneshotStatsResult {
  const query = useQuery<OneshotStats>({
    queryKey: oneshotKeys.all,
    queryFn: () => invoke<OneshotStats>("get_oneshot_stats"),
    staleTime: 15_000,
    refetchInterval: 30_000,
  });

  return {
    stats: query.data ?? null,
    loading: query.isLoading,
    error: query.error instanceof Error ? query.error.message : null,
    refresh: () => query.refetch(),
  };
}
