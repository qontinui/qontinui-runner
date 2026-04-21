/**
 * useScriptedOutputStats
 *
 * Fetch per-run (or global) aggregate counters for the scripted-output
 * ("think-in-code") subsystem. Data source is the
 * `get_scripted_output_stats` Tauri command, which aggregates rows from
 * `activity_timeline` where `source_type = 'scripted_output'`.
 *
 * Feeds the {@link ScriptedOutputPanel} widget on the LLM Analytics page.
 */

import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

/**
 * Shape returned by the `get_scripted_output_stats` Tauri command.
 *
 * Keys are camelCase because the Rust side serializes with
 * `#[serde(rename_all = "camelCase")]`.
 *
 * **Phase A events** (shipped, see `step_output/script_emitter.rs`):
 * - `cacheHit` ← `scripted_output.cache_hit`
 * - `llmOk` ← `scripted_output.llm_ok`
 * - `fallbacks[reason]` ← `scripted_output.fallback` metadata.reason
 * - `totalTokensIn` / `totalTokensOut` ← llm_ok metadata
 *
 * **Phase B events** (not yet emitted — values may be 0):
 * - `attempted` ← `scripted_output.attempted`
 * - `workerOk` ← `scripted_output.worker_ok`
 * - `bytesAvoided` ← `scripted_output.bytes_avoided` metadata.bytes_avoided
 */
export interface ScriptedOutputStats {
  attempted: number;
  cacheHit: number;
  llmOk: number;
  workerOk: number;
  bytesAvoided: number;
  fallbacks: Record<string, number>;
  totalTokensIn: number;
  totalTokensOut: number;
  /**
   * Sum of `cache_creation_tokens` across all `scripted_output.llm_ok`
   * events. Non-zero only when the warm provider's system prefix crossed
   * the minimum cacheable size and the cache was freshly populated.
   */
  cacheCreationTokens: number;
  /**
   * Sum of `cache_read_tokens` across all `scripted_output.llm_ok` events.
   * Non-zero only after the cache is populated and the ≥2s propagation
   * window has elapsed.
   */
  cacheReadTokens: number;
}

export const scriptedOutputKeys = {
  all: ["scripted-output-stats"] as const,
  forRun: (taskRunId: string | undefined) =>
    [...scriptedOutputKeys.all, taskRunId ?? "__global__"] as const,
};

export interface UseScriptedOutputStatsResult {
  stats: ScriptedOutputStats | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Fetch aggregate counters for the scripted-output subsystem.
 *
 * @param taskRunId  If provided, scopes counters to that run. When omitted
 *                   or `undefined`, aggregates across all runs (including
 *                   the "unassigned" bucket Phase A writes for task-less
 *                   calls).
 */
export function useScriptedOutputStats(taskRunId?: string): UseScriptedOutputStatsResult {
  const query = useQuery<ScriptedOutputStats>({
    queryKey: scriptedOutputKeys.forRun(taskRunId),
    queryFn: () =>
      invoke<ScriptedOutputStats>("get_scripted_output_stats", {
        // Tauri auto-converts camelCase to snake_case for the Rust side.
        taskRunId: taskRunId ?? null,
      }),
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
