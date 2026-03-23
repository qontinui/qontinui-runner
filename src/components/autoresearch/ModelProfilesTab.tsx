/**
 * ModelProfilesTab — Model capability matrix with performance and cost metrics.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface ModelProfile {
  model_id: string;
  pass_rate: number;
  mean_iterations: number;
  mean_duration_ms: number;
  avg_cost_per_run_usd: number;
  cost_per_success_usd: number;
  cost_efficiency_score: number;
  trial_count: number;
  last_updated: string;
}

export function ModelProfilesTab() {
  const [profiles, setProfiles] = useState<ModelProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<ModelProfile[]>("get_model_profiles");
      result.sort((a, b) => b.cost_efficiency_score - a.cost_efficiency_score);
      setProfiles(result);
    } catch (e) {
      console.error("Failed to load model profiles:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleRefresh = useCallback(async () => {
    try {
      setRefreshing(true);
      const result = await invoke<ModelProfile[]>("refresh_model_profiles", { days: 30 });
      result.sort((a, b) => b.cost_efficiency_score - a.cost_efficiency_score);
      setProfiles(result);
    } catch (e) {
      console.error("Failed to refresh model profiles:", e);
    } finally {
      setRefreshing(false);
    }
  }, []);

  return (
    <div className="p-4 space-y-4">
      <div className="flex gap-3 items-center">
        <h3 className="text-sm text-zinc-300 font-medium">Model Capability Matrix</h3>
        <button
          onClick={handleRefresh}
          disabled={refreshing}
          className="px-3 py-1 text-xs bg-blue-800 text-blue-200 rounded hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {refreshing ? "Refreshing..." : "Refresh"}
        </button>
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Reload
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{profiles.length} models</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : profiles.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No model profiles yet. Click "Refresh" to compute profiles from recent runs.
        </div>
      ) : (
        <div className="overflow-auto">
          <table className="w-full text-xs">
            <thead>
              <tr className="text-zinc-500 border-b border-zinc-700">
                <th className="text-left py-2 px-2">Model</th>
                <th className="text-left py-2 px-2 min-w-[140px]">Pass Rate</th>
                <th className="text-right py-2 px-2">Avg Cost</th>
                <th className="text-right py-2 px-2">Cost/Success</th>
                <th className="text-right py-2 px-2">Efficiency</th>
                <th className="text-right py-2 px-2">Trials</th>
              </tr>
            </thead>
            <tbody>
              {profiles.map((p) => {
                const passRatePct = (p.pass_rate * 100).toFixed(1);
                const barWidth = Math.min(p.pass_rate * 100, 100);
                // Green gradient based on pass rate
                const barColor =
                  p.pass_rate >= 0.8
                    ? "bg-green-500"
                    : p.pass_rate >= 0.5
                      ? "bg-green-700"
                      : "bg-green-900";

                return (
                  <tr key={p.model_id} className="border-t border-zinc-800 hover:bg-zinc-800/30">
                    <td className="py-2 px-2 text-zinc-200 font-mono">{p.model_id}</td>
                    <td className="py-2 px-2">
                      <div className="flex items-center gap-2">
                        <div className="flex-1 h-3 bg-zinc-800 rounded-full overflow-hidden">
                          <div
                            className={`h-full ${barColor} rounded-full transition-all`}
                            style={{ width: `${barWidth}%` }}
                          />
                        </div>
                        <span className="text-zinc-300 w-12 text-right">{passRatePct}%</span>
                      </div>
                    </td>
                    <td className="py-2 px-2 text-right text-zinc-300">
                      ${p.avg_cost_per_run_usd.toFixed(4)}
                    </td>
                    <td className="py-2 px-2 text-right text-zinc-300">
                      ${p.cost_per_success_usd.toFixed(4)}
                    </td>
                    <td className="py-2 px-2 text-right text-zinc-200 font-medium">
                      {p.cost_efficiency_score.toFixed(2)}
                    </td>
                    <td className="py-2 px-2 text-right text-zinc-500">{p.trial_count}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
