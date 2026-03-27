/**
 * PromptCanaryPanel — Shows active prompt template canary A/B tests with metrics and actions.
 */

import { useState } from "react";
import { usePromptCanaryList } from "../../hooks/usePromptCanary";
import type { PromptCanaryStatus, PromptCanaryMetrics } from "../../hooks/usePromptCanary";
import { CreatePromptCanaryForm } from "./CreatePromptCanaryForm";

const MIN_RUNS_FOR_VERDICT = 20;

function successRate(m: PromptCanaryMetrics): number {
  const total = m.success_count + m.failure_count;
  return total > 0 ? (m.success_count / total) * 100 : 0;
}

function avgCost(m: PromptCanaryMetrics): number {
  return m.run_count > 0 ? m.total_cost_usd / m.run_count : 0;
}

function avgLatency(m: PromptCanaryMetrics): number {
  return m.run_count > 0 ? m.total_duration_ms / m.run_count : 0;
}

function verdictColor(verdict: string | null): string {
  switch (verdict) {
    case "promote":
      return "text-green-400";
    case "rollback":
      return "text-red-400";
    case "keep":
      return "text-yellow-400";
    default:
      return "text-zinc-400";
  }
}

function statusBadge(status: string) {
  const colors: Record<string, string> = {
    active: "text-purple-400 bg-purple-900/30",
    promoted: "text-green-400 bg-green-900/30",
    rolled_back: "text-orange-400 bg-orange-900/30",
    completed: "text-blue-400 bg-blue-900/30",
  };
  return colors[status] ?? "text-zinc-400 bg-zinc-800";
}

function CanaryCard({
  canary,
  onPromote,
  onRollback,
}: {
  canary: PromptCanaryStatus;
  onPromote: (id: string) => void;
  onRollback: (id: string) => void;
}) {
  const bSR = successRate(canary.baseline_metrics);
  const cSR = successRate(canary.candidate_metrics);
  const deltaSR = cSR - bSR;
  const bCost = avgCost(canary.baseline_metrics);
  const cCost = avgCost(canary.candidate_metrics);
  const bLatency = avgLatency(canary.baseline_metrics);
  const cLatency = avgLatency(canary.candidate_metrics);
  const totalRuns = canary.baseline_metrics.run_count + canary.candidate_metrics.run_count;
  const progress = Math.min(canary.candidate_metrics.run_count / MIN_RUNS_FOR_VERDICT, 1);
  const isActive = canary.status === "active";

  return (
    <div className="bg-purple-900/20 border border-purple-800/50 rounded-lg p-4">
      {/* Header */}
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2 min-w-0">
          <span className={`text-xs px-2 py-0.5 rounded ${statusBadge(canary.status)}`}>
            {canary.status}
          </span>
          <span className="text-sm text-zinc-200 truncate">{canary.template_id}</span>
          <span className="text-xs text-zinc-500">
            v{canary.baseline_version} vs v{canary.candidate_version}
          </span>
          <span className="text-xs text-zinc-600">({canary.traffic_pct}% candidate traffic)</span>
        </div>
        {isActive && (
          <div className="flex gap-2 flex-shrink-0">
            <button
              onClick={() => onPromote(canary.id)}
              className="px-2 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
            >
              Promote
            </button>
            <button
              onClick={() => onRollback(canary.id)}
              className="px-2 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
            >
              Rollback
            </button>
          </div>
        )}
      </div>

      {/* Metrics comparison */}
      <div className="grid grid-cols-4 gap-4 text-xs mb-3">
        <div>
          <div className="text-zinc-500 mb-1">Metric</div>
          <div className="text-zinc-400">Success Rate</div>
          <div className="text-zinc-400">Avg Cost</div>
          <div className="text-zinc-400">Avg Latency</div>
        </div>
        <div>
          <div className="text-zinc-500 mb-1">Baseline (v{canary.baseline_version})</div>
          <div className="text-zinc-200">{bSR.toFixed(1)}%</div>
          <div className="text-zinc-200">${bCost.toFixed(4)}</div>
          <div className="text-zinc-200">{bLatency.toFixed(0)}ms</div>
        </div>
        <div>
          <div className="text-zinc-500 mb-1">Candidate (v{canary.candidate_version})</div>
          <div className="text-zinc-200">{cSR.toFixed(1)}%</div>
          <div className="text-zinc-200">${cCost.toFixed(4)}</div>
          <div className="text-zinc-200">{cLatency.toFixed(0)}ms</div>
        </div>
        <div>
          <div className="text-zinc-500 mb-1">Delta</div>
          <div
            className={`font-medium ${deltaSR > 0 ? "text-green-400" : deltaSR < -2 ? "text-red-400" : "text-zinc-300"}`}
          >
            {deltaSR > 0 ? "+" : ""}
            {deltaSR.toFixed(1)}pp
          </div>
          <div
            className={`${cCost < bCost ? "text-green-400" : cCost > bCost ? "text-red-400" : "text-zinc-300"}`}
          >
            {bCost > 0 ? `${(((cCost - bCost) / bCost) * 100).toFixed(1)}%` : "-"}
          </div>
          <div
            className={`${cLatency < bLatency ? "text-green-400" : cLatency > bLatency ? "text-red-400" : "text-zinc-300"}`}
          >
            {bLatency > 0 ? `${(((cLatency - bLatency) / bLatency) * 100).toFixed(1)}%` : "-"}
          </div>
        </div>
      </div>

      {/* Statistical evaluation */}
      <div className="flex flex-wrap gap-4 text-xs mb-3">
        {canary.p_value != null && (
          <div>
            <div className="text-zinc-500">p-value</div>
            <div className="text-zinc-300">{canary.p_value.toFixed(4)}</div>
          </div>
        )}
        <div>
          <div className="text-zinc-500">Verdict</div>
          <div className={`font-medium ${verdictColor(canary.verdict)}`}>
            {canary.verdict ?? "pending"}
          </div>
        </div>
        <div>
          <div className="text-zinc-500">Total Runs</div>
          <div className="text-zinc-300">{totalRuns}</div>
        </div>
      </div>

      {/* Progress bar toward minimum sample size */}
      {isActive && (
        <div>
          <div className="flex justify-between text-xs text-zinc-500 mb-1">
            <span>Candidate sample progress</span>
            <span>
              {canary.candidate_metrics.run_count}/{MIN_RUNS_FOR_VERDICT} min runs
            </span>
          </div>
          <div className="h-1.5 bg-zinc-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-purple-500 rounded-full transition-all"
              style={{ width: `${progress * 100}%` }}
            />
          </div>
        </div>
      )}
    </div>
  );
}

export function PromptCanaryPanel() {
  const { canaries, loading, error, refresh, promote, rollback } = usePromptCanaryList();
  const [showCreate, setShowCreate] = useState(false);

  const handleCreated = () => {
    refresh();
  };

  return (
    <div className="space-y-4" data-tutorial-id="prompt-canary-panel">
      <div className="flex items-center justify-between">
        <div className="text-sm text-zinc-400">Prompt Canary A/B Tests</div>
        <div className="flex items-center gap-2">
          <button
            onClick={refresh}
            className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1"
          >
            Refresh
          </button>
          <button
            onClick={() => setShowCreate(!showCreate)}
            className="px-3 py-1 text-xs bg-purple-700 text-purple-100 rounded hover:bg-purple-600"
          >
            {showCreate ? "Hide Form" : "Create A/B Test"}
          </button>
        </div>
      </div>

      {showCreate && (
        <CreatePromptCanaryForm
          onCreated={handleCreated}
          onClose={() => setShowCreate(false)}
        />
      )}

      {error && (
        <div className="text-xs text-red-400 bg-red-900/20 border border-red-800/50 rounded px-3 py-2">
          {error}
        </div>
      )}

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading canary tests...</div>
      ) : canaries.length === 0 ? (
        <div className="text-zinc-500 text-sm py-4 text-center">
          No prompt canary A/B tests found. Create one to compare prompt versions.
        </div>
      ) : (
        <div className="space-y-3">
          {canaries.map((c) => (
            <CanaryCard key={c.id} canary={c} onPromote={promote} onRollback={rollback} />
          ))}
        </div>
      )}
    </div>
  );
}
