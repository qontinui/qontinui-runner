/**
 * useFeedbackScores
 *
 * TanStack Query hook for fetching feedback scores for an execution run.
 * Returns scores, computed summary stats, and loading/error state.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getApiBase, tracedFetch } from "../lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type FeedbackScoreSource = "manual" | "automated" | "llm_judge";

export interface FeedbackScore {
  id: string;
  run_id: string;
  name: string;
  value: number; // 0-1
  source: FeedbackScoreSource;
  reason?: string | null;
  created_at: string;
}

export interface FeedbackScoresSummary {
  /** Arithmetic mean of all score values */
  averageScore: number;
  /** Total number of scores */
  totalCount: number;
  /** Count grouped by source */
  countBySource: Record<FeedbackScoreSource, number>;
}

export interface CreateFeedbackScoreInput {
  name: string;
  value: number;
  reason?: string;
}

export interface UseFeedbackScoresReturn {
  scores: FeedbackScore[];
  summary: FeedbackScoresSummary | null;
  loading: boolean;
  error: Error | null;
  refetch: () => void;
  createScore: ReturnType<typeof useMutation<FeedbackScore, Error, CreateFeedbackScoreInput>>;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const feedbackScoreKeys = {
  all: ["feedbackScores"] as const,
  byRun: (runId: string) => [...feedbackScoreKeys.all, runId] as const,
};

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

async function fetchFeedbackScores(runId: string): Promise<FeedbackScore[]> {
  const res = await tracedFetch(`${getApiBase()}/api/v1/execution-runs/${runId}/feedback-scores`);
  if (!res.ok) {
    throw new Error(`Failed to fetch feedback scores (HTTP ${res.status})`);
  }
  return res.json();
}

// ---------------------------------------------------------------------------
// Summary computation
// ---------------------------------------------------------------------------

function computeSummary(scores: FeedbackScore[]): FeedbackScoresSummary | null {
  if (scores.length === 0) return null;

  const total = scores.reduce((sum, s) => sum + s.value, 0);
  const countBySource: Record<FeedbackScoreSource, number> = {
    manual: 0,
    automated: 0,
    llm_judge: 0,
  };
  for (const s of scores) {
    countBySource[s.source] = (countBySource[s.source] ?? 0) + 1;
  }

  return {
    averageScore: total / scores.length,
    totalCount: scores.length,
    countBySource,
  };
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Fetches feedback scores for the given execution run and computes summary
 * statistics. Scores are cached for 30 seconds.
 */
export function useFeedbackScores(runId: string | null | undefined): UseFeedbackScoresReturn {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: feedbackScoreKeys.byRun(runId ?? ""),
    queryFn: () => fetchFeedbackScores(runId!),
    enabled: !!runId,
    staleTime: 30_000,
  });

  const scores = query.data ?? [];
  const summary = scores.length > 0 ? computeSummary(scores) : null;

  const createScore = useMutation<FeedbackScore, Error, CreateFeedbackScoreInput>({
    mutationFn: async (score) => {
      const res = await fetch(`${getApiBase()}/api/v1/feedback-scores`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          target_type: "run",
          target_id: runId,
          name: score.name,
          value: score.value,
          source: "manual",
          reason: score.reason || null,
        }),
      });
      if (!res.ok) throw new Error("Failed to create score");
      return res.json();
    },
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: feedbackScoreKeys.byRun(runId ?? "") }),
  });

  return {
    scores,
    summary,
    loading: query.isLoading,
    error: query.error instanceof Error ? query.error : null,
    refetch: query.refetch,
    createScore,
  };
}
