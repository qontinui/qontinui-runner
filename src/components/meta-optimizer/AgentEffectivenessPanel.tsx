/**
 * AgentEffectivenessPanel — per-agent metrics for the Meta-Optimizer Progress tab.
 *
 * Shows each pipeline agent's current effectiveness metrics (success rate,
 * avg duration, avg cost, tokens) so you can measure the impact of
 * meta-optimizer recommendations on individual agents.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AgentAggregate {
  agent_type: string;
  run_count: number;
  avg_duration_ms: number;
  avg_cost_usd: number;
  success_count: number;
  failure_count: number;
  avg_tokens_in: number;
  avg_tokens_out: number;
}

const AGENT_LABELS: Record<string, string> = {
  spec_analyst: "Spec Analyst",
  locator: "Locator",
  implementer: "Implementer",
  verifier: "Verifier",
};

const AGENT_COLORS: Record<string, string> = {
  spec_analyst: "border-l-blue-500",
  locator: "border-l-cyan-500",
  implementer: "border-l-amber-500",
  verifier: "border-l-green-500",
};

function successRate(a: AgentAggregate): number {
  const total = a.success_count + a.failure_count;
  return total > 0 ? a.success_count / total : 0;
}

function SuccessBar({ rate }: { rate: number }) {
  const pct = rate * 100;
  const color = pct >= 80 ? "bg-green-500" : pct >= 50 ? "bg-amber-500" : "bg-red-500";
  return (
    <div className="flex items-center gap-2">
      <div className="w-24 h-1.5 bg-zinc-700 rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${color}`}
          style={{ width: `${Math.min(pct, 100)}%` }}
        />
      </div>
      <span className="text-xs text-zinc-300 font-mono w-12 text-right">{pct.toFixed(0)}%</span>
    </div>
  );
}

export function AgentEffectivenessPanel() {
  const [agents, setAgents] = useState<AgentAggregate[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const result = await invoke<AgentAggregate[]>("get_agent_effectiveness", {
        limit: 500,
      });
      setAgents(result);
    } catch {
      // No traces yet
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    load();
  }, [load]);

  if (loading) {
    return null;
  }

  if (agents.length === 0) {
    return null;
  }

  // Sort by agent type for consistent ordering
  const sorted = [...agents].sort(
    (a, b) =>
      Object.keys(AGENT_LABELS).indexOf(a.agent_type) -
      Object.keys(AGENT_LABELS).indexOf(b.agent_type),
  );

  // Totals for summary row
  const totalRuns = sorted.reduce((s, a) => s + a.run_count, 0);
  const totalSuccess = sorted.reduce((s, a) => s + a.success_count, 0);
  const totalFailure = sorted.reduce((s, a) => s + a.failure_count, 0);
  const totalCost = sorted.reduce((s, a) => s + a.avg_cost_usd * a.run_count, 0);
  const overallSuccessRate =
    totalSuccess + totalFailure > 0 ? totalSuccess / (totalSuccess + totalFailure) : 0;

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div className="text-sm text-zinc-400">Per-Agent Effectiveness</div>
        <button onClick={load} className="text-xs text-zinc-500 hover:text-zinc-300">
          Refresh
        </button>
      </div>

      {/* Summary row */}
      <div className="bg-zinc-800/50 rounded-lg px-4 py-2 mb-3 border border-zinc-700/50 flex items-center gap-6 text-xs text-zinc-400">
        <span>
          <span className="text-zinc-200 font-medium">{totalRuns}</span> total invocations
        </span>
        <span>
          Overall success:{" "}
          <span className="text-zinc-200 font-medium">
            {(overallSuccessRate * 100).toFixed(0)}%
          </span>
        </span>
        <span>
          Total cost: <span className="text-zinc-200 font-medium">${totalCost.toFixed(2)}</span>
        </span>
      </div>

      {/* Agent cards */}
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
        {sorted.map((a) => {
          const rate = successRate(a);
          const label = AGENT_LABELS[a.agent_type] ?? a.agent_type;
          const borderColor = AGENT_COLORS[a.agent_type] ?? "border-l-zinc-500";

          return (
            <div
              key={a.agent_type}
              className={`bg-zinc-800 rounded-lg p-3 border border-zinc-700 border-l-2 ${borderColor}`}
            >
              <div className="flex items-center justify-between mb-2">
                <div className="text-xs font-medium text-zinc-200">{label}</div>
                <div className="text-[10px] text-zinc-500">{a.run_count} runs</div>
              </div>

              {/* Success rate bar */}
              <SuccessBar rate={rate} />

              {/* Metrics grid */}
              <div className="grid grid-cols-2 gap-x-3 gap-y-1 mt-2 text-[11px]">
                <div className="text-zinc-500">Avg Duration</div>
                <div className="text-zinc-300 text-right font-mono">
                  {(a.avg_duration_ms / 1000).toFixed(1)}s
                </div>
                <div className="text-zinc-500">Avg Cost</div>
                <div className="text-zinc-300 text-right font-mono">
                  ${a.avg_cost_usd.toFixed(4)}
                </div>
                <div className="text-zinc-500">Tokens In</div>
                <div className="text-zinc-300 text-right font-mono">
                  {a.avg_tokens_in.toFixed(0)}
                </div>
                <div className="text-zinc-500">Tokens Out</div>
                <div className="text-zinc-300 text-right font-mono">
                  {a.avg_tokens_out.toFixed(0)}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
