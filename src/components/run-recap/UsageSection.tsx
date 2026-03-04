import React, { useEffect, useState } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface PhaseUsage {
  phase: string;
  stage_index: number | null;
  iteration: number | null;
  model_used: string | null;
  provider_used: string | null;
  input_tokens: number;
  output_tokens: number;
  cost_cents: number;
  duration_ms: number | null;
}

interface UsageData {
  task_run_id: string;
  phases: PhaseUsage[];
  totals: {
    input_tokens: number;
    output_tokens: number;
    cost_cents: number;
  };
}

export function UsageSection({ taskRunId }: { taskRunId: string }) {
  const [data, setData] = useState<UsageData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    tracedFetch(`${getApiBase()}/task-runs/${taskRunId}/usage`)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then(setData)
      .catch((e) => setError(e.message));
  }, [taskRunId]);

  if (error) return null; // Silently hide if no usage data
  if (!data || data.phases.length === 0) return null;

  const formatTokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));

  return (
    <div className="bg-zinc-800/50 rounded-md p-3">
      <h3 className="text-xs font-medium text-zinc-300 mb-2">Token Usage</h3>
      <div className="overflow-x-auto">
        <table className="w-full text-[11px]">
          <thead>
            <tr className="text-zinc-500 text-left">
              <th className="pb-1 pr-3 font-medium">Phase</th>
              <th className="pb-1 pr-3 font-medium">Model</th>
              <th className="pb-1 pr-3 font-medium text-right">Input</th>
              <th className="pb-1 pr-3 font-medium text-right">Output</th>
              <th className="pb-1 font-medium text-right">Duration</th>
            </tr>
          </thead>
          <tbody>
            {data.phases.map((p, i) => (
              <tr key={i} className="text-zinc-300 border-t border-zinc-700/50">
                <td className="py-1 pr-3">
                  <span className="capitalize">{p.phase}</span>
                  {p.stage_index != null && (
                    <span className="text-zinc-500 ml-1">S{p.stage_index + 1}</span>
                  )}
                  {p.iteration != null && (
                    <span className="text-zinc-500 ml-1">#{p.iteration}</span>
                  )}
                </td>
                <td className="py-1 pr-3 text-zinc-400 truncate max-w-[140px]">
                  {p.model_used ?? "—"}
                </td>
                <td className="py-1 pr-3 text-right tabular-nums">
                  {formatTokens(p.input_tokens)}
                </td>
                <td className="py-1 pr-3 text-right tabular-nums">
                  {formatTokens(p.output_tokens)}
                </td>
                <td className="py-1 text-right text-zinc-400 tabular-nums">
                  {p.duration_ms != null ? `${(p.duration_ms / 1000).toFixed(1)}s` : "—"}
                </td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr className="text-zinc-200 border-t border-zinc-600 font-medium">
              <td className="pt-1.5 pr-3" colSpan={2}>
                Total
              </td>
              <td className="pt-1.5 pr-3 text-right tabular-nums">
                {formatTokens(data.totals.input_tokens)}
              </td>
              <td className="pt-1.5 pr-3 text-right tabular-nums">
                {formatTokens(data.totals.output_tokens)}
              </td>
              <td className="pt-1.5"></td>
            </tr>
          </tfoot>
        </table>
      </div>
    </div>
  );
}
