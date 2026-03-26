/**
 * PromptOptimizationTab — Meta-prompt optimizer dashboard.
 *
 * Shows prompt groups ranked by failure rate, active canaries with progress,
 * evolution history timeline, and manual trigger button.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface PromptGroupMetrics {
  phase: string;
  agent_type: string;
  sample_count: number;
  mean_score: number;
  failure_rate: number;
  mean_iterations: number;
  mean_cost: number;
  latest_prompt_text: string;
}

interface PromptEvolutionEntry {
  id: string;
  agent_type: string;
  parent_variant_id: string | null;
  variant_id: string;
  recommendation_id: string | null;
  critique: string | null;
  changes_summary: string | null;
  canary_verdict: string | null;
  score_before: number | null;
  score_after: number | null;
  created_at: string;
}

interface OptimizationStatus {
  prompt_groups: PromptGroupMetrics[];
  active_canaries: PromptEvolutionEntry[];
  evolution_history: PromptEvolutionEntry[];
}

export function PromptOptimizationTab() {
  const [status, setStatus] = useState<OptimizationStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [triggering, setTriggering] = useState(false);
  const [expandedGroup, setExpandedGroup] = useState<string | null>(null);
  const [expandedEvolution, setExpandedEvolution] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<OptimizationStatus>(
        "get_prompt_optimization_status"
      );
      setStatus(result);
    } catch (e) {
      console.error("Failed to load prompt optimization status:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleTrigger = async () => {
    try {
      setTriggering(true);
      await invoke("trigger_meta_optimizer", {
        optimizerType: "meta_prompt",
      });
      setTimeout(load, 2000);
    } catch (e) {
      console.error("Failed to trigger meta-prompt optimizer:", e);
    } finally {
      setTriggering(false);
    }
  };

  if (loading) {
    return (
      <div className="p-4 text-zinc-400">Loading prompt optimization data...</div>
    );
  }

  if (!status) {
    return (
      <div className="p-4 text-zinc-500">No prompt optimization data available.</div>
    );
  }

  const { prompt_groups, active_canaries, evolution_history } = status;

  return (
    <div className="p-4 space-y-6">
      {/* Header with trigger button */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-zinc-100">
            Prompt Optimization
          </h2>
          <p className="text-sm text-zinc-400">
            LLM-based iterative prompt rewriting with canary A/B testing
          </p>
        </div>
        <button
          onClick={handleTrigger}
          disabled={triggering}
          className="px-4 py-2 text-sm bg-blue-600 text-white rounded hover:bg-blue-500 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {triggering ? "Triggering..." : "Trigger Optimization"}
        </button>
      </div>

      {/* Active canaries */}
      {active_canaries.length > 0 && (
        <div className="border border-yellow-700/50 bg-yellow-900/10 rounded-lg p-4">
          <h3 className="text-sm font-semibold text-yellow-400 mb-2">
            Active Canaries ({active_canaries.length})
          </h3>
          <div className="space-y-2">
            {active_canaries.map((c) => (
              <div
                key={c.id}
                className="flex items-center justify-between text-sm"
              >
                <span className="text-zinc-300">
                  {c.agent_type} &mdash; variant {c.variant_id}
                </span>
                <span className="text-yellow-400">
                  Awaiting verdict
                  {c.score_before != null &&
                    ` (baseline: ${(c.score_before * 100).toFixed(0)}%)`}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Prompt groups ranked by failure rate */}
      <div>
        <h3 className="text-sm font-semibold text-zinc-300 mb-2">
          Prompt Groups by Failure Rate
        </h3>
        {prompt_groups.length === 0 ? (
          <p className="text-sm text-zinc-500">
            No prompt groups found. Run some workflows first.
          </p>
        ) : (
          <div className="space-y-1">
            {prompt_groups.map((g) => {
              const key = `${g.phase}/${g.agent_type}`;
              const isExpanded = expandedGroup === key;
              const isBad = g.failure_rate > 0.3;
              return (
                <div key={key}>
                  <button
                    onClick={() =>
                      setExpandedGroup(isExpanded ? null : key)
                    }
                    className="w-full flex items-center justify-between px-3 py-2 text-sm bg-zinc-800/50 rounded hover:bg-zinc-800 text-left"
                  >
                    <span className="text-zinc-200">
                      {g.phase}/{g.agent_type}
                    </span>
                    <div className="flex items-center gap-4 text-xs">
                      <span
                        className={
                          isBad ? "text-red-400" : "text-zinc-400"
                        }
                      >
                        Fail: {(g.failure_rate * 100).toFixed(0)}%
                      </span>
                      <span className="text-zinc-400">
                        Score: {(g.mean_score * 100).toFixed(0)}%
                      </span>
                      <span className="text-zinc-500">
                        n={g.sample_count}
                      </span>
                      <span className="text-zinc-500">
                        {isExpanded ? "\u25B2" : "\u25BC"}
                      </span>
                    </div>
                  </button>
                  {isExpanded && (
                    <div className="ml-4 mt-1 p-3 bg-zinc-900 rounded text-xs space-y-1">
                      <div className="text-zinc-400">
                        Mean iterations: {g.mean_iterations.toFixed(1)} |
                        Mean cost: ${g.mean_cost.toFixed(4)}
                      </div>
                      {g.latest_prompt_text && (
                        <div className="mt-2">
                          <div className="text-zinc-500 mb-1">
                            Latest prompt:
                          </div>
                          <pre className="whitespace-pre-wrap text-zinc-400 max-h-48 overflow-auto bg-zinc-950 p-2 rounded">
                            {g.latest_prompt_text.slice(0, 500)}
                            {g.latest_prompt_text.length > 500 && "..."}
                          </pre>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Evolution history */}
      <div>
        <h3 className="text-sm font-semibold text-zinc-300 mb-2">
          Evolution History
        </h3>
        {evolution_history.length === 0 ? (
          <p className="text-sm text-zinc-500">
            No prompt rewrites attempted yet.
          </p>
        ) : (
          <div className="space-y-2">
            {evolution_history.map((e) => {
              const isExpanded = expandedEvolution === e.id;
              return (
                <div key={e.id}>
                  <button
                    onClick={() =>
                      setExpandedEvolution(isExpanded ? null : e.id)
                    }
                    className="w-full flex items-center justify-between px-3 py-2 bg-zinc-800/50 rounded text-sm text-left hover:bg-zinc-800"
                  >
                    <div>
                      <span className="text-zinc-200">{e.agent_type}</span>
                      {e.changes_summary && (
                        <span className="text-zinc-500 ml-2">
                          &mdash; {e.changes_summary.slice(0, 80)}
                          {e.changes_summary.length > 80 && "..."}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-3 text-xs">
                      {e.score_before != null && (
                        <span className="text-zinc-400">
                          {(e.score_before * 100).toFixed(0)}%
                        </span>
                      )}
                      <span className="text-zinc-500">&rarr;</span>
                      {e.score_after != null ? (
                        <span
                          className={
                            e.score_after > (e.score_before ?? 0)
                              ? "text-green-400"
                              : "text-red-400"
                          }
                        >
                          {(e.score_after * 100).toFixed(0)}%
                        </span>
                      ) : (
                        <span className="text-zinc-500">?</span>
                      )}
                      <VerdictBadge verdict={e.canary_verdict} />
                      <span className="text-zinc-600">
                        {new Date(e.created_at).toLocaleDateString()}
                      </span>
                      <span className="text-zinc-500">
                        {isExpanded ? "\u25B2" : "\u25BC"}
                      </span>
                    </div>
                  </button>
                  {isExpanded && (
                    <div className="ml-4 mt-1 p-3 bg-zinc-900 rounded text-xs space-y-2">
                      {e.critique && (
                        <div>
                          <div className="text-zinc-500 font-semibold mb-1">
                            Critique
                          </div>
                          <pre className="whitespace-pre-wrap text-zinc-400">
                            {e.critique}
                          </pre>
                        </div>
                      )}
                      {e.changes_summary && (
                        <div>
                          <div className="text-zinc-500 font-semibold mb-1">
                            Changes
                          </div>
                          <pre className="whitespace-pre-wrap text-zinc-400">
                            {e.changes_summary}
                          </pre>
                        </div>
                      )}
                      <div className="text-zinc-600">
                        Variant: {e.variant_id}
                        {e.parent_variant_id &&
                          ` (parent: ${e.parent_variant_id})`}
                        {e.recommendation_id &&
                          ` | Rec: ${e.recommendation_id}`}
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function VerdictBadge({ verdict }: { verdict: string | null }) {
  if (!verdict) {
    return (
      <span className="px-2 py-0.5 rounded bg-yellow-900/30 text-yellow-400 text-xs">
        pending
      </span>
    );
  }
  const colors: Record<string, string> = {
    adopt: "bg-green-900/30 text-green-400",
    reject: "bg-red-900/30 text-red-400",
    inconclusive: "bg-zinc-700/50 text-zinc-400",
  };
  return (
    <span
      className={`px-2 py-0.5 rounded text-xs ${colors[verdict] ?? "bg-zinc-700/50 text-zinc-400"}`}
    >
      {verdict}
    </span>
  );
}
