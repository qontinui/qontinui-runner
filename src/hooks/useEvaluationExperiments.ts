/**
 * useEvaluationExperiments
 *
 * TanStack Query hooks for evaluation experiment CRUD and results.
 * Communicates with the runner backend via REST API endpoints.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/hooks/useApiHelpers";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type ExperimentStatus = "pending" | "running" | "completed" | "failed";

export interface EvaluationExperiment {
  id: string;
  dataset_id: string;
  name: string;
  description?: string;
  status: ExperimentStatus;
  completed_count: number;
  total_count: number;
  created_at: string;
  updated_at: string;
}

export interface ExperimentResultScore {
  name: string;
  value: number;
}

export interface ExperimentResult {
  id: string;
  experiment_id: string;
  input: string;
  output: string;
  expected_output?: string;
  scores: ExperimentResultScore[];
  duration_ms: number;
  cost_cents: number;
  tokens_used: number;
  created_at: string;
}

export interface ExperimentResultsSummary {
  avg_scores: Record<string, number>;
  total_cost_cents: number;
  total_tokens: number;
  total_duration_ms: number;
  result_count: number;
}

export interface ExperimentResultsResponse {
  results: ExperimentResult[];
  summary: ExperimentResultsSummary;
}

export interface CreateExperimentRequest {
  dataset_id: string;
  name: string;
  description?: string;
  prompt_variant_id?: string;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const evaluationExperimentKeys = {
  all: ["evaluation-experiments"] as const,
  list: (datasetId: string) => [...evaluationExperimentKeys.all, "list", datasetId] as const,
  results: (id: string) => [...evaluationExperimentKeys.all, "results", id] as const,
};

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useEvaluationExperiments(datasetId: string | null) {
  const queryClient = useQueryClient();

  // -- List experiments for a dataset ---------------------------------------
  const experimentsQuery = useQuery({
    queryKey: evaluationExperimentKeys.list(datasetId ?? ""),
    queryFn: () =>
      apiFetch<EvaluationExperiment[]>(`/api/v1/evaluation/experiments?dataset_id=${datasetId}`),
    enabled: !!datasetId,
    staleTime: 15_000,
    refetchInterval: 30_000,
  });

  // -- Create experiment ----------------------------------------------------
  const createExperiment = useMutation({
    mutationFn: (req: CreateExperimentRequest) =>
      apiFetch<EvaluationExperiment>("/api/v1/evaluation/experiments", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    onSuccess: () => {
      if (datasetId) {
        queryClient.invalidateQueries({ queryKey: evaluationExperimentKeys.list(datasetId) });
      }
    },
  });

  return {
    experiments: experimentsQuery.data ?? [],
    isLoading: experimentsQuery.isLoading,
    error: experimentsQuery.error?.message ?? null,
    refresh: () => {
      if (datasetId) {
        queryClient.invalidateQueries({ queryKey: evaluationExperimentKeys.list(datasetId) });
      }
    },
    createExperiment,
  };
}

// ---------------------------------------------------------------------------
// Results hook (per-experiment)
// ---------------------------------------------------------------------------

export function useExperimentResults(experimentId: string | null) {
  const resultsQuery = useQuery({
    queryKey: evaluationExperimentKeys.results(experimentId ?? ""),
    queryFn: () =>
      apiFetch<ExperimentResultsResponse>(`/api/v1/evaluation/experiments/${experimentId}/results`),
    enabled: !!experimentId,
    staleTime: 15_000,
    refetchInterval: 30_000,
  });

  return {
    results: resultsQuery.data?.results ?? [],
    summary: resultsQuery.data?.summary ?? null,
    isLoading: resultsQuery.isLoading,
    error: resultsQuery.error?.message ?? null,
  };
}
