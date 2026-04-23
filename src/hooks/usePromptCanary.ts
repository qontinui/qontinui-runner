/**
 * usePromptCanary
 *
 * Hook for managing prompt template canary A/B tests.
 * Provides create, status query, promote, and rollback operations via Tauri commands.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface PromptCanaryMetrics {
  success_count: number;
  failure_count: number;
  total_cost_usd: number;
  total_duration_ms: number;
  run_count: number;
}

export interface PromptCanaryStatus {
  id: string;
  template_id: string;
  baseline_version: number;
  candidate_version: number;
  traffic_pct: number;
  status: string;
  baseline_metrics: PromptCanaryMetrics;
  candidate_metrics: PromptCanaryMetrics;
  p_value: number | null;
  verdict: string | null;
  created_at: string;
  updated_at: string;
}

interface UsePromptCanaryListReturn {
  canaries: PromptCanaryStatus[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  promote: (canaryId: string) => Promise<void>;
  rollback: (canaryId: string) => Promise<void>;
}

interface UseCreatePromptCanaryReturn {
  createCanary: (
    templateId: string,
    baselineVersion: number,
    candidateVersion: number,
    trafficPct: number,
  ) => Promise<void>;
  creating: boolean;
  error: string | null;
  success: boolean;
  reset: () => void;
}

export function usePromptCanaryList(): UsePromptCanaryListReturn {
  const [canaries, setCanaries] = useState<PromptCanaryStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const refresh = useCallback(async () => {
    try {
      setError(null);
      const data = await invoke<PromptCanaryStatus[]>("get_prompt_canary_status");
      setCanaries(data);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void refresh();
    });
    intervalRef.current = setInterval(refresh, 30000);
    return () => {
      cancelled = true;
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [refresh]);

  const promote = useCallback(
    async (canaryId: string) => {
      try {
        await invoke("promote_prompt_canary", { canaryId });
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refresh],
  );

  const rollback = useCallback(
    async (canaryId: string) => {
      try {
        await invoke("rollback_prompt_canary", { canaryId });
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refresh],
  );

  return { canaries, loading, error, refresh, promote, rollback };
}

export function useCreatePromptCanary(): UseCreatePromptCanaryReturn {
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const reset = useCallback(() => {
    setError(null);
    setSuccess(false);
  }, []);

  const createCanary = useCallback(
    async (
      templateId: string,
      baselineVersion: number,
      candidateVersion: number,
      trafficPct: number,
    ) => {
      try {
        setCreating(true);
        setError(null);
        setSuccess(false);
        await invoke("create_prompt_canary", {
          templateId,
          baselineVersion,
          candidateVersion,
          trafficPct,
        });
        setSuccess(true);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setCreating(false);
      }
    },
    [],
  );

  return { createCanary, creating, error, success, reset };
}
