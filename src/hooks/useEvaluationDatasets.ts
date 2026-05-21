/**
 * useEvaluationDatasets
 *
 * TanStack Query hooks for evaluation dataset CRUD operations.
 * Communicates with the runner backend via REST API endpoints.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/hooks/useApiHelpers";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface EvaluationDataset {
  id: string;
  name: string;
  description?: string;
  item_count: number;
  version: number;
  created_at: string;
  updated_at: string;
}

export interface EvaluationDatasetItem {
  id: string;
  dataset_id: string;
  input: string;
  expected_output: string;
  metadata?: Record<string, unknown>;
  created_at: string;
}

export interface CreateDatasetRequest {
  name: string;
  description?: string;
}

export interface AddItemsRequest {
  items: Array<{
    input: string;
    expected_output: string;
    metadata?: Record<string, unknown>;
  }>;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const evaluationDatasetKeys = {
  all: ["evaluation-datasets"] as const,
  list: () => [...evaluationDatasetKeys.all, "list"] as const,
  detail: (id: string) => [...evaluationDatasetKeys.all, "detail", id] as const,
  items: (id: string) => [...evaluationDatasetKeys.all, "items", id] as const,
};

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useEvaluationDatasets() {
  const queryClient = useQueryClient();

  // -- List datasets --------------------------------------------------------
  // The `/api/v1/evaluation/datasets` REST endpoints are not yet implemented
  // in the runner backend. When the endpoint returns 404, do not retry —
  // TanStack Query's default 3-retries-with-backoff would otherwise spam the
  // console with "API 404" errors. Once the backend lands, the retry policy
  // returns to handling transient 5xx normally.
  const datasetsQuery = useQuery({
    queryKey: evaluationDatasetKeys.list(),
    queryFn: () => apiFetch<EvaluationDataset[]>("/api/v1/evaluation/datasets"),
    staleTime: 30_000,
    retry: (failureCount, error) => {
      if (error instanceof Error && error.message.includes("API 404")) {
        return false;
      }
      return failureCount < 3;
    },
  });

  // -- Create dataset -------------------------------------------------------
  const createDataset = useMutation({
    mutationFn: (req: CreateDatasetRequest) =>
      apiFetch<EvaluationDataset>("/api/v1/evaluation/datasets", {
        method: "POST",
        body: JSON.stringify(req),
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.list() });
    },
  });

  // -- Delete dataset -------------------------------------------------------
  const deleteDataset = useMutation({
    mutationFn: (id: string) =>
      apiFetch<void>(`/api/v1/evaluation/datasets/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.list() });
    },
  });

  return {
    datasets: datasetsQuery.data ?? [],
    isLoading: datasetsQuery.isLoading,
    error: datasetsQuery.error?.message ?? null,
    refresh: () => queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.all }),
    createDataset,
    deleteDataset,
  };
}

// ---------------------------------------------------------------------------
// Items hook (per-dataset)
// ---------------------------------------------------------------------------

export function useDatasetItems(datasetId: string | null) {
  const queryClient = useQueryClient();

  const itemsQuery = useQuery({
    queryKey: evaluationDatasetKeys.items(datasetId ?? ""),
    queryFn: () =>
      apiFetch<EvaluationDatasetItem[]>(`/api/v1/evaluation/datasets/${datasetId}/items`),
    enabled: !!datasetId,
    staleTime: 30_000,
  });

  const addItems = useMutation({
    mutationFn: (req: AddItemsRequest) =>
      apiFetch<EvaluationDatasetItem[]>(`/api/v1/evaluation/datasets/${datasetId}/items`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    onSuccess: () => {
      if (datasetId) {
        queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.items(datasetId) });
        queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.list() });
      }
    },
  });

  const deleteItem = useMutation({
    mutationFn: (itemId: string) =>
      apiFetch<void>(`/api/v1/evaluation/datasets/items/${itemId}`, { method: "DELETE" }),
    onSuccess: () => {
      if (datasetId) {
        queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.items(datasetId) });
        queryClient.invalidateQueries({ queryKey: evaluationDatasetKeys.list() });
      }
    },
  });

  return {
    items: itemsQuery.data ?? [],
    isLoading: itemsQuery.isLoading,
    error: itemsQuery.error?.message ?? null,
    addItems,
    deleteItem,
  };
}
