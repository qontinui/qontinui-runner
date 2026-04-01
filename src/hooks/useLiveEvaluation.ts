/**
 * useLiveEvaluation
 *
 * Hook for on-demand LLM-as-judge evaluation via the `evaluate_with_io` Tauri command.
 * Also fetches saved eval specs via `get_eval_specs`.
 */

import { useState, useCallback } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface AssertionResult {
  assertion_type: string;
  passed: boolean;
  actual_value: number;
  threshold_value: number;
  message: string;
}

export interface EvalSpec {
  id: string;
  name: string;
  target_agent: string | null;
  test_cases: unknown[];
  thresholds: unknown;
}

export interface EvaluateRequest {
  specJson: string;
  input: string;
  output: string;
  context?: string;
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useLiveEvaluation() {
  const [results, setResults] = useState<AssertionResult[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch saved eval specs
  const specsQuery = useQuery({
    queryKey: ["eval-specs"],
    queryFn: () => invoke<EvalSpec[]>("get_eval_specs", { targetAgent: null }),
    staleTime: 60_000,
  });

  const evaluate = useCallback(async ({ specJson, input, output, context }: EvaluateRequest) => {
    setLoading(true);
    setError(null);
    setResults(null);

    try {
      const res = await invoke<AssertionResult[]>("evaluate_with_io", {
        evalSpecJson: specJson,
        input,
        output,
        context: context || null,
      });
      setResults(res);
      return res;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      return null;
    } finally {
      setLoading(false);
    }
  }, []);

  const reset = useCallback(() => {
    setResults(null);
    setError(null);
  }, []);

  return {
    evaluate,
    results,
    loading,
    error,
    reset,
    specs: specsQuery.data ?? [],
    specsLoading: specsQuery.isLoading,
  };
}
