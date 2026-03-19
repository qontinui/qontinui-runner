/**
 * RecommendationsTab — Table of meta-optimizer recommendations with Apply/Reject/Rollback.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface RecommendationOutcome {
  verdict: string;
  success_rate_delta?: number;
  duration_delta_ms?: number;
  cost_delta_usd?: number;
  metrics_before?: Record<string, number>;
  metrics_after?: Record<string, number>;
}

interface CascadeEffect {
  affected_agent: string;
  before_success_rate: number;
  after_success_rate: number;
  delta: number;
  sample_size_before: number;
  sample_size_after: number;
}

interface Recommendation {
  id: string;
  optimizer_type: string;
  recommendation_type: string;
  target_agent: string | null;
  title: string;
  description: string;
  current_value: string | null;
  recommended_value: string | null;
  evidence: string | null;
  confidence: number;
  status: string;
  applied_at: string | null;
  outcome_after_apply: string | null;
  optimizer_run_id: string | null;
  created_at: string;
}

const STATUS_COLORS: Record<string, string> = {
  pending: "text-yellow-400 bg-yellow-900/30",
  applied: "text-green-400 bg-green-900/30",
  rejected: "text-zinc-500 bg-zinc-800/50",
  rolled_back: "text-orange-400 bg-orange-900/30",
  superseded: "text-zinc-600 bg-zinc-800/30",
  canary: "text-purple-400 bg-purple-900/30",
};

const VERDICT_BADGES: Record<string, { label: string; className: string }> = {
  improved: { label: "Improved", className: "text-green-400 bg-green-900/30" },
  regressed: { label: "Regressed", className: "text-red-400 bg-red-900/30" },
  neutral: { label: "Neutral", className: "text-zinc-400 bg-zinc-800/50" },
  insufficient_data: { label: "Pending", className: "text-yellow-400 bg-yellow-900/30" },
};

const TYPE_LABELS: Record<string, string> = {
  pipeline_prompt: "Prompt",
  architecture: "Architecture",
  generation_template: "Generation",
};

export function RecommendationsTab() {
  const [recs, setRecs] = useState<Recommendation[]>([]);
  const [filterType, setFilterType] = useState<string>("all");
  const [filterStatus, setFilterStatus] = useState<string>("all");
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [cascadeEffects, setCascadeEffects] = useState<Record<string, CascadeEffect[]>>({});

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<Recommendation[]>("get_meta_optimizer_recommendations", {
        optimizerType: filterType === "all" ? null : filterType,
        status: filterStatus === "all" ? null : filterStatus,
      });
      setRecs(result);
    } catch (e) {
      console.error("Failed to load recommendations:", e);
    } finally {
      setLoading(false);
    }
  }, [filterType, filterStatus]);

  useEffect(() => { load(); }, [load]);

  const handleAction = async (id: string, action: "apply" | "reject" | "rollback" | "canary") => {
    try {
      if (action === "canary") {
        await invoke("start_canary_rollout", { recommendationId: id, percentage: 10 });
      } else {
        const cmd = action === "apply"
          ? "apply_meta_optimizer_recommendation"
          : action === "reject"
            ? "reject_meta_optimizer_recommendation"
            : "rollback_meta_optimizer_recommendation";
        await invoke(cmd, { recommendationId: id });
      }
      load();
    } catch (e) {
      console.error(`Failed to ${action} recommendation:`, e);
    }
  };

  const handleReevaluate = async (id: string) => {
    try {
      await invoke("reevaluate_recommendation_outcome", { recommendationId: id });
      load();
    } catch (e) {
      console.error("Re-evaluate failed:", e);
    }
  };

  const loadCascade = async (recId: string) => {
    if (cascadeEffects[recId]) return;
    try {
      const effects = await invoke<CascadeEffect[]>("get_agent_cascade_effect", {
        recommendationId: recId,
      });
      setCascadeEffects((prev) => ({ ...prev, [recId]: effects }));
    } catch {
      // silently fail
    }
  };

  return (
    <div className="p-4 space-y-4">
      {/* Filters */}
      <div className="flex gap-3 items-center">
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={filterType}
          onChange={(e) => setFilterType(e.target.value)}
        >
          <option value="all">All Types</option>
          <option value="pipeline_prompt">Prompt</option>
          <option value="architecture">Architecture</option>
          <option value="generation_template">Generation</option>
        </select>
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={filterStatus}
          onChange={(e) => setFilterStatus(e.target.value)}
        >
          <option value="all">All Statuses</option>
          <option value="pending">Pending</option>
          <option value="applied">Applied</option>
          <option value="canary">Canary</option>
          <option value="rejected">Rejected</option>
          <option value="rolled_back">Rolled Back</option>
        </select>
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{recs.length} recommendations</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : recs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No recommendations yet. Run workflows to accumulate data, then trigger the meta-optimizer.
        </div>
      ) : (
        <div className="space-y-2">
          {recs.map((rec) => (
            <div
              key={rec.id}
              className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
            >
              <div
                className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-zinc-800/50"
                onClick={() => {
                  const newExpanded = expanded === rec.id ? null : rec.id;
                  setExpanded(newExpanded);
                  if (newExpanded && rec.status === "applied" && rec.target_agent) {
                    loadCascade(rec.id);
                  }
                }}
              >
                <span className={`text-xs px-2 py-0.5 rounded ${STATUS_COLORS[rec.status] || ""}`}>
                  {rec.status}
                </span>
                {rec.status === "applied" && rec.outcome_after_apply && (() => {
                  try {
                    const outcome: RecommendationOutcome = JSON.parse(rec.outcome_after_apply);
                    const badge = VERDICT_BADGES[outcome.verdict];
                    if (badge) {
                      return (
                        <span className={`text-xs px-1.5 py-0.5 rounded ${badge.className}`}>
                          {badge.label}
                        </span>
                      );
                    }
                  } catch { /* ignore */ }
                  return null;
                })()}
                <span className="text-xs text-zinc-500">
                  {TYPE_LABELS[rec.optimizer_type] || rec.optimizer_type}
                </span>
                {rec.target_agent && (
                  <span className="text-xs text-zinc-600">{rec.target_agent}</span>
                )}
                <span className="text-sm text-zinc-200 flex-1">{rec.title}</span>
                <span className="text-xs text-zinc-500">
                  {(rec.confidence * 100).toFixed(0)}% conf
                </span>
                <span className="text-xs text-zinc-600">
                  {new Date(rec.created_at).toLocaleDateString()}
                </span>
              </div>

              {expanded === rec.id && (
                <div className="px-4 pb-4 space-y-3 border-t border-zinc-800">
                  <p className="text-sm text-zinc-300 mt-3">{rec.description}</p>

                  {rec.recommended_value && (
                    <div className="space-y-1">
                      <div className="text-xs text-zinc-500">Recommended Value</div>
                      <pre className="text-xs text-zinc-300 bg-zinc-800/50 p-2 rounded overflow-auto max-h-48">
                        {rec.recommended_value}
                      </pre>
                    </div>
                  )}

                  {rec.evidence && (
                    <div className="space-y-1">
                      <div className="text-xs text-zinc-500">Evidence</div>
                      <pre className="text-xs text-zinc-400 bg-zinc-800/30 p-2 rounded overflow-auto max-h-32">
                        {rec.evidence}
                      </pre>
                    </div>
                  )}

                  {/* Outcome details for applied recs */}
                  {rec.status === "applied" && rec.outcome_after_apply && (() => {
                    try {
                      const outcome: RecommendationOutcome = JSON.parse(rec.outcome_after_apply);
                      return (
                        <div className="bg-zinc-800/50 rounded p-3 space-y-2">
                          <div className="text-xs text-zinc-500">Outcome After Apply</div>
                          <div className="flex gap-4 text-xs">
                            {outcome.success_rate_delta != null && (
                              <div>
                                <span className="text-zinc-500">Success Rate: </span>
                                <span className={outcome.success_rate_delta > 0 ? "text-green-400" : outcome.success_rate_delta < -2 ? "text-red-400" : "text-zinc-300"}>
                                  {outcome.success_rate_delta > 0 ? "+" : ""}{outcome.success_rate_delta.toFixed(1)}pp
                                </span>
                              </div>
                            )}
                            {outcome.duration_delta_ms != null && (
                              <div>
                                <span className="text-zinc-500">Duration: </span>
                                <span className="text-zinc-300">
                                  {outcome.duration_delta_ms > 0 ? "+" : ""}{outcome.duration_delta_ms.toFixed(0)}ms
                                </span>
                              </div>
                            )}
                            {outcome.cost_delta_usd != null && (
                              <div>
                                <span className="text-zinc-500">Cost: </span>
                                <span className="text-zinc-300">
                                  {outcome.cost_delta_usd > 0 ? "+" : ""}${outcome.cost_delta_usd.toFixed(4)}
                                </span>
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    } catch { return null; }
                  })()}

                  {/* Cascade impact for applied recs with target_agent */}
                  {rec.status === "applied" && rec.target_agent && cascadeEffects[rec.id] && cascadeEffects[rec.id].length > 0 && (
                    <div className="bg-zinc-800/50 rounded p-3 space-y-2">
                      <div className="text-xs text-zinc-500">Cascade Impact</div>
                      <table className="w-full text-xs">
                        <thead>
                          <tr className="text-zinc-500">
                            <th className="text-left py-1">Agent</th>
                            <th className="text-right py-1">Before</th>
                            <th className="text-right py-1">After</th>
                            <th className="text-right py-1">Delta</th>
                          </tr>
                        </thead>
                        <tbody>
                          {cascadeEffects[rec.id].map((ce) => (
                            <tr key={ce.affected_agent} className="border-t border-zinc-700/50">
                              <td className="py-1 text-zinc-300">{ce.affected_agent}</td>
                              <td className="py-1 text-right text-zinc-400">{ce.before_success_rate.toFixed(1)}%</td>
                              <td className="py-1 text-right text-zinc-400">{ce.after_success_rate.toFixed(1)}%</td>
                              <td className={`py-1 text-right ${ce.delta > 0 ? "text-green-400" : ce.delta < -2 ? "text-red-400" : "text-zinc-400"}`}>
                                {ce.delta > 0 ? "+" : ""}{ce.delta.toFixed(1)}pp
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  )}

                  <div className="flex gap-2 pt-2">
                    {rec.status === "pending" && (
                      <>
                        <button
                          onClick={() => handleAction(rec.id, "apply")}
                          className="px-3 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
                        >
                          Apply
                        </button>
                        <button
                          onClick={() => handleAction(rec.id, "canary")}
                          className="px-3 py-1 text-xs bg-purple-800 text-purple-200 rounded hover:bg-purple-700"
                        >
                          Canary 10%
                        </button>
                        <button
                          onClick={() => handleAction(rec.id, "reject")}
                          className="px-3 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                        >
                          Reject
                        </button>
                      </>
                    )}
                    {rec.status === "applied" && (
                      <>
                        <button
                          onClick={() => handleAction(rec.id, "rollback")}
                          className="px-3 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
                        >
                          Rollback
                        </button>
                        <button
                          onClick={() => handleReevaluate(rec.id)}
                          className="px-3 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                        >
                          Re-evaluate
                        </button>
                      </>
                    )}
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
