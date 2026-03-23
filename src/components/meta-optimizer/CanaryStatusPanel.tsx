/**
 * CanaryStatusPanel — Shows active canary rollouts with baseline vs canary metrics.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

interface CanaryRollout {
  id: string;
  recommendation_id: string;
  recommendation_title?: string;
  target_agent?: string | null;
  percentage: number;
  status: string;
  start_date: string;
  end_date: string | null;
  baseline_run_count: number;
  canary_run_count: number;
  baseline_metrics_json: string;
  canary_metrics_json: string;
  created_at: string;
}

interface CanaryMetrics {
  success_count: number;
  failure_count: number;
  total_cost_usd: number;
  total_duration_ms: number;
  p_value?: number | null;
  confidence_interval?: [number, number] | null;
  effect_size?: number | null;
  cost_delta_pct?: number | null;
  duration_delta_pct?: number | null;
}

const MIN_CANARY_RUNS = 20;

function parseMetrics(json: string): CanaryMetrics {
  try {
    return JSON.parse(json) as CanaryMetrics;
  } catch {
    return { success_count: 0, failure_count: 0, total_cost_usd: 0, total_duration_ms: 0 };
  }
}

function successRate(m: CanaryMetrics): number {
  const total = m.success_count + m.failure_count;
  return total > 0 ? (m.success_count / total) * 100 : 0;
}

export function CanaryStatusPanel() {
  const [canaries, setCanaries] = useState<CanaryRollout[]>([]);
  const [loading, setLoading] = useState(true);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const load = useCallback(async () => {
    try {
      const data = await invoke<CanaryRollout[]>("get_canary_rollouts");
      setCanaries(data);
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    intervalRef.current = setInterval(load, 30000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [load]);

  const handlePromote = async (canaryId: string) => {
    try {
      await invoke("promote_canary_rollout", { canaryId });
      await load();
    } catch (e) {
      console.error("Promote failed:", e);
    }
  };

  const handleRollback = async (canaryId: string) => {
    try {
      await invoke("rollback_canary_rollout", { canaryId });
      await load();
    } catch (e) {
      console.error("Rollback failed:", e);
    }
  };

  if (loading || canaries.length === 0) {
    return null;
  }

  return (
    <div>
      <div className="text-sm text-zinc-400 mb-2">Active Canary Rollouts</div>
      <div className="space-y-3">
        {canaries.map((c) => {
          const baseline = parseMetrics(c.baseline_metrics_json);
          const canary = parseMetrics(c.canary_metrics_json);
          const baselineSR = successRate(baseline);
          const canarySR = successRate(canary);
          const delta = canarySR - baselineSR;
          const progress = Math.min(c.canary_run_count / MIN_CANARY_RUNS, 1);

          return (
            <div key={c.id} className="bg-purple-900/20 border border-purple-800/50 rounded-lg p-4">
              <div className="flex items-center justify-between mb-3">
                <div className="flex items-center gap-2">
                  <span className="text-xs px-2 py-0.5 rounded text-purple-400 bg-purple-900/30">
                    canary {c.percentage}%
                  </span>
                  <span className="text-sm text-zinc-200 truncate max-w-xs">
                    {c.recommendation_title ?? c.recommendation_id}
                  </span>
                  {c.target_agent && (
                    <span className="text-xs text-zinc-500">({c.target_agent})</span>
                  )}
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={() => handlePromote(c.id)}
                    className="px-2 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
                  >
                    Promote
                  </button>
                  <button
                    onClick={() => handleRollback(c.id)}
                    className="px-2 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
                  >
                    Rollback
                  </button>
                </div>
              </div>

              {/* Metrics comparison */}
              <div className="grid grid-cols-3 gap-4 text-xs mb-3">
                <div>
                  <div className="text-zinc-500">Baseline Success</div>
                  <div className="text-zinc-200 font-medium">{baselineSR.toFixed(1)}%</div>
                  <div className="text-zinc-500">{c.baseline_run_count} runs</div>
                </div>
                <div>
                  <div className="text-zinc-500">Canary Success</div>
                  <div className="text-zinc-200 font-medium">{canarySR.toFixed(1)}%</div>
                  <div className="text-zinc-500">{c.canary_run_count} runs</div>
                </div>
                <div>
                  <div className="text-zinc-500">Delta</div>
                  <div
                    className={`font-medium ${
                      delta > 0 ? "text-green-400" : delta < -2 ? "text-red-400" : "text-zinc-300"
                    }`}
                  >
                    {delta > 0 ? "+" : ""}
                    {delta.toFixed(1)}pp
                  </div>
                </div>
              </div>

              {/* Statistical details */}
              {(canary.p_value != null ||
                canary.confidence_interval != null ||
                canary.effect_size != null ||
                canary.cost_delta_pct != null ||
                canary.duration_delta_pct != null) && (
                <div className="flex flex-wrap gap-4 text-xs mb-3">
                  {canary.p_value != null && (
                    <div>
                      <div className="text-zinc-500">p-value</div>
                      <div className="text-zinc-300">{canary.p_value.toFixed(4)}</div>
                    </div>
                  )}
                  {canary.confidence_interval != null && (
                    <div>
                      <div className="text-zinc-500">Confidence Interval</div>
                      <div className="text-zinc-300">
                        [{canary.confidence_interval[0].toFixed(2)},{" "}
                        {canary.confidence_interval[1].toFixed(2)}]
                      </div>
                    </div>
                  )}
                  {canary.effect_size != null && (
                    <div>
                      <div className="text-zinc-500">Effect Size</div>
                      <div className="text-zinc-300">{canary.effect_size.toFixed(3)}</div>
                    </div>
                  )}
                  {canary.cost_delta_pct != null && (
                    <div>
                      <div className="text-zinc-500">Cost Delta</div>
                      <div
                        className={
                          canary.cost_delta_pct < 0
                            ? "text-green-400"
                            : canary.cost_delta_pct > 0
                              ? "text-red-400"
                              : "text-zinc-300"
                        }
                      >
                        {canary.cost_delta_pct > 0 ? "+" : ""}
                        {canary.cost_delta_pct.toFixed(1)}%
                      </div>
                    </div>
                  )}
                  {canary.duration_delta_pct != null && (
                    <div>
                      <div className="text-zinc-500">Duration Delta</div>
                      <div
                        className={
                          canary.duration_delta_pct < 0
                            ? "text-green-400"
                            : canary.duration_delta_pct > 0
                              ? "text-red-400"
                              : "text-zinc-300"
                        }
                      >
                        {canary.duration_delta_pct > 0 ? "+" : ""}
                        {canary.duration_delta_pct.toFixed(1)}%
                      </div>
                    </div>
                  )}
                </div>
              )}

              {/* Progress bar toward minimum sample size */}
              <div>
                <div className="flex justify-between text-xs text-zinc-500 mb-1">
                  <span>Sample progress</span>
                  <span>
                    {c.canary_run_count}/{MIN_CANARY_RUNS} min runs
                  </span>
                </div>
                <div className="h-1.5 bg-zinc-800 rounded-full overflow-hidden">
                  <div
                    className="h-full bg-purple-500 rounded-full transition-all"
                    style={{ width: `${progress * 100}%` }}
                  />
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
